;;; triage-daemon-test.el --- Scratch-daemon ERT tests for the triage client -*- lexical-binding: t; -*-

;;; Commentary:

;; Integration tests use a scratch `triage serve' process.  Each test gets a
;; temporary HOME, database and failing tmux stub, so the daemon never sees the
;; live inbox, real tmux panes or a dsh host.  Set TRIAGE_BIN to choose the
;; binary.  Otherwise tests use ../target/debug/triage from emacs/.  Tests skip
;; when that binary is absent.

;;; Code:

(require 'ert)
(require 'cl-lib)
(require 'triage)

(defconst triage-test--emacs-dir
  (file-truename
   (expand-file-name ".."
                     (file-name-directory
                      (or load-file-name buffer-file-name default-directory))))
  "Directory of the Emacs package, the parent of the test directory.")

(defun triage-test--binary ()
  "Return the triage binary to test, or nil when it is not built."
  (let ((bin (if (getenv "TRIAGE_BIN")
                 (expand-file-name (getenv "TRIAGE_BIN"))
               (expand-file-name "../target/debug/triage" triage-test--emacs-dir))))
    (and (file-executable-p bin) bin)))

(defun triage-test--free-port ()
  "Return a TCP port on 127.0.0.1 that nothing is listening on."
  (let* ((server (make-network-process :name "triage-port" :server t
                                       :host "127.0.0.1" :service t :noquery t))
         (port (process-contact server :service)))
    (delete-process server)
    port))

(defun triage-test--reset-queue ()
  "Forget the queue state that an earlier test left behind."
  (setq triage-queue-cards nil
        triage-queue-counts nil
        triage-queue-version nil
        triage-queue-initialized nil
        triage-queue-connection 'connecting
        triage-queue-unreachable-since nil
        triage-queue--refreshing nil
        triage-queue--dirty nil
        triage-queue--retry-delay 1)
  (clrhash triage-queue--inflight)
  (clrhash triage-queue--drafts))

(defun triage-test--await-daemon (proc)
  "Wait until the daemon PROC answers the queue route."
  (let ((deadline (+ (float-time) 5))
        ready)
    (while (and (not ready) (process-live-p proc) (< (float-time) deadline))
      (condition-case nil
          (progn
            (triage-api-request-sync 'get "/v1/queue" :timeout 1)
            (setq ready t))
        (triage-api-error (sleep-for 0.1))))
    (should ready)))

(defun triage-test--teardown (proc log dir)
  "Stop the daemon PROC and clean up LOG, DIR and every triage buffer."
  (dolist (who (copy-sequence triage-queue--consumers))
    (triage-queue-unsubscribe who))
  (when (and proc (process-live-p proc))
    (delete-process proc))
  (dolist (buffer (buffer-list))
    (when (string-prefix-p "*triage" (buffer-name buffer))
      (kill-buffer buffer)))
  (kill-buffer log)
  (delete-directory dir t))

(defun triage-test--call-with-daemon (body)
  "Start a scratch daemon, call BODY with `triage-url' bound, then stop it."
  (let ((bin (triage-test--binary)))
    (unless bin
      (ert-skip "triage binary not built; run cargo build or set TRIAGE_BIN"))
    (let* ((dir (make-temp-file "triage-el-" t))
           (stub (expand-file-name "stub" dir))
           (tmux (expand-file-name "tmux" stub))
           (port (triage-test--free-port))
           (log (generate-new-buffer " *triage-daemon-test*"))
           (proc nil))
      (unwind-protect
          (progn
            (make-directory stub)
            (write-region "#!/bin/sh\nexit 1\n" nil tmux nil 'silent)
            (set-file-modes tmux #o755)
            (triage-test--reset-queue)
            (let ((process-environment
                   (list (concat "PATH=" stub)
                         (concat "HOME=" dir)
                         (concat "TMPDIR=" dir)
                         (concat "XDG_DATA_HOME=" dir)
                         (concat "DSH_HOST_FILE="
                                 (expand-file-name "no-dsh-host.json" dir))))
                  (default-directory (file-truename default-directory))
                  (triage-url (format "http://127.0.0.1:%d" port)))
              (setq proc
                    (make-process
                     :name "triage-daemon" :buffer log :noquery t
                     :command (list bin "serve"
                                    "--db" (expand-file-name "triage.db" dir)
                                    "--listen" (format "127.0.0.1:%d" port))))
              (triage-test--await-daemon proc)
              (funcall body)))
        (triage-test--teardown proc log dir)))))

(defmacro triage-test-with-daemon (&rest body)
  "Run BODY against a fresh scratch triage daemon."
  (declare (indent 0))
  `(triage-test--call-with-daemon (lambda () ,@body)))

(defun triage-test--wait (pred secs)
  "Run the event loop until PRED is non-nil or SECS pass, then assert PRED."
  (let ((deadline (+ (float-time) secs)))
    (while (and (not (funcall pred)) (< (float-time) deadline))
      (accept-process-output nil 0.05))
    (should (funcall pred))))

(defun triage-test--await-stream ()
  "Wait until the SSE stream has its headers, so no event is missed."
  (triage-test--wait
   (lambda ()
     (and triage-queue--stream (process-get triage-queue--stream :triage-body)))
   5))

(defun triage-test--subscribe (who)
  "Subscribe WHO to the queue and wait until the stream is connected."
  (triage-queue-subscribe who)
  (triage-test--wait (lambda () (eq triage-queue-connection 'connected)) 5)
  (triage-test--await-stream))

(defun triage-test--await-cards (count)
  "Wait until the queue has COUNT cards."
  (triage-test--wait (lambda () (= (length triage-queue-cards) count)) 5))

(defun triage-test--idle ()
  "Return non-nil when the queue is empty and no action is in flight."
  (and (null triage-queue-cards)
       (zerop (hash-table-count triage-queue--inflight))))

(defun triage-test--buffer-has (name &rest strings)
  "Return non-nil when buffer NAME includes all of STRINGS."
  (let ((buffer (get-buffer name)))
    (and buffer
         (with-current-buffer buffer
           (cl-every (lambda (string)
                       (string-match-p (regexp-quote string) (buffer-string)))
                     strings)))))

(defun triage-test--messages ()
  "Return the *Messages* log and the current echo message as one string."
  (concat (with-current-buffer (messages-buffer) (buffer-string))
          "\n"
          (or (current-message) "")))

(defun triage-test--ingest (event)
  "POST the CloudEvent EVENT to the scratch daemon."
  (triage-api-request-sync 'post "/v1/ingest" :body event))

(defun triage-daemon-test--finding (id title)
  "Return a triage.finding CloudEvent with ID, TITLE and two options."
  `((specversion . "1.0") (id . ,id) (source . "test://emacs")
    (type . "triage.finding")
    (data . ((title . ,title)
             (options . ,(vector '((label . "Do as recommended") (recommended . t))
                                 '((label . "Investigate first"))))))))

(defun triage-daemon-test--info (id body)
  "Return a triage.info CloudEvent with ID and Markdown BODY."
  `((specversion . "1.0") (id . ,id) (source . "test://emacs")
    (type . "triage.info")
    (data . ((body . ,body)))))

(ert-deftest triage-daemon-test-stream-refresh ()
  "An SSE queue event refreshes the queue with no manual refresh."
  (triage-test-with-daemon
    (triage-test--subscribe 'test)
    (triage-test--ingest (triage-daemon-test--finding "el-1" "Emacs check finding"))
    (triage-test--await-cards 1)
    (should (equal (alist-get 'title (car triage-queue-cards))
                   "Emacs check finding"))))

(ert-deftest triage-daemon-test-inbox-render-and-skip ()
  "The inbox renders a finding, and skipping it empties the deck."
  (triage-test-with-daemon
    (triage-test--ingest (triage-daemon-test--finding "el-1" "Emacs check finding"))
    (triage)
    (triage-test--wait
     (lambda ()
       (triage-test--buffer-has "*triage*" "Emacs check finding" "Waiting on you"
                                "1 Do as recommended" "Dispatch"))
     5)
    (let ((id (alist-get 'id (car triage-queue-cards))))
      (with-current-buffer "*triage*"
        (triage-inbox-skip))
      (triage-test--wait #'triage-test--idle 5)
      (let* ((response (triage-api-request-sync 'get "/v1/history?limit=5"))
             (row (car (alist-get 'cards response))))
        (should (equal (alist-get 'id (alist-get 'card row)) id))
        (should (equal (alist-get 'action (alist-get 'swipe row)) "skipped"))))
    (triage-test--wait (lambda () (triage-test--buffer-has "*triage*" "Inbox zero")) 5)))

(ert-deftest triage-daemon-test-info-heading-and-ack ()
  "An info card takes its title from its body heading and acks with RET."
  (triage-test-with-daemon
    (triage-test--ingest (triage-daemon-test--info "el-2" "# Heading line\n\nBody"))
    (triage)
    (triage-test--wait
     (lambda ()
       (triage-test--buffer-has "*triage*" "Heading line" "a/RET Acknowledge"))
     5)
    (with-current-buffer "*triage*"
      (triage-inbox-primary))
    (triage-test--wait #'triage-test--idle 5)))

(ert-deftest triage-daemon-test-context-section-hook ()
  "The context view keeps its Magit root and card sections after loading."
  (triage-test-with-daemon
    (triage-test--subscribe 'context)
    (triage-test--ingest (triage-daemon-test--info "el-context" "# Context report"))
    (triage-test--await-cards 1)
    (triage-context-open (alist-get 'id (car triage-queue-cards)))
    (let ((buffer (get-buffer "*triage context*")))
      (triage-test--wait
       (lambda () (triage-test--buffer-has "*triage context*" "Context report" "This card"))
       5)
      (with-current-buffer buffer
        (goto-char (point-min))
        (let ((section (magit-section-at)))
          (should (eieio-object-p section))
          (should (eq (oref section parent) magit-root-section)))
        (magit-section-post-command-hook)
        (should (eieio-object-p (magit-current-section)))))))

(ert-deftest triage-daemon-test-reload ()
  "Reload returns a handled card as a fresh copy and refuses a second copy."
  (triage-test-with-daemon
    (triage-test--subscribe 'test)
    (triage-test--ingest (triage-daemon-test--finding "el-1" "Reload finding"))
    (triage-test--await-cards 1)
    (let ((id (alist-get 'id (car triage-queue-cards)))
          reloaded)
      (triage-api-request-sync 'post (triage-api-card-path id "skip"))
      (triage-test--wait #'triage-test--idle 5)
      (let ((response (triage-api-request-sync 'post (triage-api-card-path id "reload"))))
        (should (eq (alist-get 'ok response) t))
        (setq reloaded (alist-get 'card response)))
      (should (string-prefix-p "reload:" reloaded))
      (triage-test--wait
       (lambda ()
         (and (= (length triage-queue-cards) 1)
              (equal (alist-get 'id (car triage-queue-cards)) reloaded)))
       5)
      (let ((err (should-error
                  (triage-api-request-sync 'post (triage-api-card-path id "reload"))
                  :type 'triage-api-error)))
        (should (eql (cadr err) 409))))))

(ert-deftest triage-daemon-test-failed-dispatch-returns-card ()
  "A failed launch returns the card to the queue with the error and title."
  (triage-test-with-daemon
    (triage-test--subscribe 'test)
    (triage-test--ingest (triage-daemon-test--finding "el-1" "Emacs dispatch finding"))
    (triage-test--await-cards 1)
    (let* ((card (car triage-queue-cards))
           (id (alist-get 'id card)))
      (triage-queue-act card "dispatch"
                        '((mode . "act") (note . :null) (new_window . :false)))
      (triage-test--wait
       (lambda ()
         (and (triage-queue-find id)
              (zerop (hash-table-count triage-queue--inflight))))
       5)
      (should (string-match-p "! [^\n]* · Emacs dispatch finding"
                              (triage-test--messages))))))

(ert-deftest triage-daemon-test-mode-line ()
  "The mode-line counter shows one queued card."
  (triage-test-with-daemon
    (triage-test--ingest (triage-daemon-test--finding "el-1" "Mode line finding"))
    (unwind-protect
        (progn
          (triage-mode-line-mode 1)
          (triage-test--await-cards 1)
          (should (equal (substring-no-properties (triage-mode-line--string))
                         " triage:1")))
      (triage-mode-line-mode -1))))

(provide 'triage-daemon-test)
;;; triage-daemon-test.el ends here
