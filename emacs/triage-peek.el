;;; triage-peek.el --- Live view of a triage pane -*- lexical-binding: t; -*-

;;; Commentary:

;; A peek buffer polls its pane every two seconds.  The reader can send
;; text to a pane that waits for input, toggle fit, and jump to the pane.

;;; Code:

(require 'ansi-color)
(require 'cl-lib)
(require 'triage-api)
(require 'triage-card)

(defvar triage-peek-fit-default 'fit
  "Fit mode for new peek buffers, `fit' or `readable'.
Toggling fit in one peek also sets this, so the choice persists.")

(defvar-local triage-peek--pane nil
  "Pane id that this peek buffer reads.")

(defvar-local triage-peek--dsh-p nil
  "Non-nil when this peek buffer reads a dsh session.")

(defvar-local triage-peek--target nil
  "Target of the card behind this peek buffer, or nil.")

(defvar-local triage-peek--name nil
  "Name shown in the header line.")

(defvar-local triage-peek--state "connecting"
  "Pane state last reported by the daemon.")

(defvar-local triage-peek--size nil
  "Pane size as (WIDTH . HEIGHT), or nil when unknown.")

(defvar-local triage-peek--error nil
  "Message of the last failed fetch, or nil.")

(defvar-local triage-peek--fetching nil
  "Non-nil while a fetch is in flight.")

(defvar-local triage-peek--token 0
  "Count of opens, so an answer to an earlier open is ignored.")

(defvar-local triage-peek--timer nil
  "Timer that refreshes this buffer.")

(defvar-local triage-peek--fit triage-peek-fit-default
  "Current fit mode, `fit' or `readable'.")

(defun triage-peek--nonempty (value)
  "Return VALUE when it is a non-empty string, else nil."
  (and (stringp value) (not (string-empty-p value)) value))

(defun triage-peek--header-line ()
  "Return the header line of the current peek buffer."
  (if triage-peek--error
      (format "Peek failed: %s" triage-peek--error)
    (format "Peek %s · %s%s · %s"
            triage-peek--name
            triage-peek--state
            (if triage-peek--size
                (format " · %d×%d"
                        (car triage-peek--size) (cdr triage-peek--size))
              "")
            (if (eq triage-peek--fit 'fit) "Fit" "Readable"))))

(defun triage-peek--fit-scale ()
  "Return the text scale that fits the pane width into the window.
Readable mode, or a buffer without a window or size, gets 0.  Fit tries
0 down to -6 and takes the first level that fits."
  (let ((win (get-buffer-window (current-buffer) t)))
    (if (and (eq triage-peek--fit 'fit) win triage-peek--size)
        (let ((avail (window-body-width win t))
              (pane-px (* (car triage-peek--size) (default-font-width))))
          (cl-loop for n from 0 downto -6
                   when (<= (* pane-px (expt 1.2 n)) avail)
                   return n
                   finally return -6))
      0)))

(defun triage-peek--apply-fit ()
  "Set the text scale of the current peek buffer for its fit mode."
  (text-scale-set (triage-peek--fit-scale)))

(defun triage-peek--cancel-timer ()
  "Cancel the refresh timer of the current peek buffer."
  (when (timerp triage-peek--timer)
    (cancel-timer triage-peek--timer))
  (setq triage-peek--timer nil))

(defun triage-peek--show (data)
  "Show the pane DATA in the current peek buffer, keeping the reader's place."
  (let* ((text (or (alist-get 'text data) ""))
         (width (alist-get 'width data))
         (height (alist-get 'height data))
         (at-end (= (point) (point-max)))
         (line (line-number-at-pos))
         (column (current-column))
         (inhibit-read-only t))
    (setq triage-peek--state (or (alist-get 'state data) "unknown")
          triage-peek--size (and (integerp width) (integerp height)
                                 (> width 0) (> height 0)
                                 (cons width height))
          triage-peek--error nil)
    (erase-buffer)
    (insert (ansi-color-apply text))
    (if at-end
        (goto-char (point-max))
      (goto-char (point-min))
      (forward-line (1- line))
      (move-to-column column))
    (triage-peek--apply-fit)))

(defun triage-peek--fail (message)
  "Mark the current peek buffer unreachable, with MESSAGE as the reason."
  (setq triage-peek--state "unreachable"
        triage-peek--error message))

(defun triage-peek--settle (buffer token fn)
  "Run FN in BUFFER when TOKEN still names the open that asked.
A fetch that answers after a reopen does nothing."
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (when (eql token triage-peek--token)
        (setq triage-peek--fetching nil)
        (funcall fn)))))

(defun triage-peek--refresh (buffer)
  "Fetch pane text for BUFFER while a window displays it."
  (when (and (buffer-live-p buffer) (get-buffer-window buffer t))
    (with-current-buffer buffer
      (unless triage-peek--fetching
        (setq triage-peek--fetching t)
        (let ((token triage-peek--token))
          (triage-api-request
           'get (triage-api-session-path triage-peek--pane "peek")
           :timeout 10
           :then (lambda (data)
                   (triage-peek--settle
                    buffer token (lambda () (triage-peek--show data))))
           :else (lambda (_status msg)
                   (triage-peek--settle
                    buffer token (lambda () (triage-peek--fail msg))))))))))

(defun triage-peek-jump-pane (pane &optional target)
  "Jump to PANE, or open the dsh session on the desktop when PANE is dsh.
TARGET, when non-nil, names the pane in the echo area."
  (if (string-prefix-p "dsh:" pane)
      (triage-api-request
       'post "/v1/dsh/open"
       :then (lambda (_) (message "Opened dsh on the desktop."))
       :else (lambda (_status msg)
               (message "%s" (propertize (format "Open failed: %s" msg)
                                         'face 'error))))
    (triage-api-request
     'post (triage-api-session-path pane "jump")
     :then (lambda (_) (message "Jumped to %s" (or target pane)))
     :else (lambda (_status msg)
             (message "%s" (propertize msg 'face 'error))))))

(defun triage-peek-open (card)
  "Open a live peek of the pane behind CARD."
  (let* ((pane (or (triage-card-pane card)
                   (user-error "This card has no pane to peek")))
         (source (alist-get 'source card))
         (dsh-p (string-prefix-p "dsh:" pane))
         (target (triage-peek--nonempty (alist-get 'target source)))
         (name (or target
                   (and dsh-p (triage-peek--nonempty (alist-get 'label source)))
                   pane))
         (buffer (get-buffer-create (format "*triage peek: %s*" name))))
    (with-current-buffer buffer
      (unless (derived-mode-p 'triage-peek-mode)
        (triage-peek-mode))
      (triage-peek--cancel-timer)
      (setq triage-peek--pane pane
            triage-peek--dsh-p dsh-p
            triage-peek--target target
            triage-peek--name name
            triage-peek--state "connecting"
            triage-peek--size nil
            triage-peek--error nil
            triage-peek--fetching nil
            triage-peek--fit triage-peek-fit-default
            triage-peek--token (1+ triage-peek--token)))
    (pop-to-buffer buffer)
    (with-current-buffer buffer
      (setq triage-peek--timer
            (run-at-time 2 2 #'triage-peek--refresh buffer)))
    (triage-peek--refresh buffer)
    buffer))

(defun triage-peek-send ()
  "Send typed text to the pane while it waits for input."
  (interactive)
  (unless (equal triage-peek--state "waiting")
    (user-error "Sending unlocks when the pane is waiting for input"))
  (let ((text (read-string (if triage-peek--dsh-p
                               "Send to the dsh session: "
                             "Send to the pane: "))))
    (when (string-empty-p (string-trim text))
      (user-error "Type text to send"))
    (let ((buffer (current-buffer))
          (dsh-p triage-peek--dsh-p))
      (triage-api-request
       'post (triage-api-session-path triage-peek--pane "send")
       :body `((text . ,text))
       :then (lambda (_)
               (message (if dsh-p
                            "Sent to the dsh session."
                          "Sent to the pane."))
               (triage-peek--refresh buffer))
       :else (lambda (_status msg)
               (message "%s" (propertize (format "Send failed: %s" msg)
                                         'face 'error)))))))

(defun triage-peek-jump ()
  "Jump to the pane of this peek, or open dsh when it is a dsh session."
  (interactive)
  (triage-peek-jump-pane triage-peek--pane triage-peek--target))

(defun triage-peek-toggle-fit ()
  "Toggle between fitting the pane width and readable text."
  (interactive)
  (setq triage-peek--fit (if (eq triage-peek--fit 'fit) 'readable 'fit)
        triage-peek-fit-default triage-peek--fit)
  (triage-peek--apply-fit))

(defun triage-peek-refresh ()
  "Fetch the pane text now."
  (interactive)
  (triage-peek--refresh (current-buffer)))

(defun triage-peek-quit ()
  "Close the peek buffer and stop its refresh timer."
  (interactive)
  (quit-window t))

(defvar-keymap triage-peek-mode-map
  :parent special-mode-map
  "s" #'triage-peek-send
  "RET" #'triage-peek-send
  "f" #'triage-peek-toggle-fit
  "g" #'triage-peek-refresh
  "J" #'triage-peek-jump
  "q" #'triage-peek-quit)

(define-derived-mode triage-peek-mode special-mode "Triage-Peek"
  "Major mode for a live view of one triage pane."
  (setq-local truncate-lines t
              header-line-format '(:eval (triage-peek--header-line)))
  (add-hook 'kill-buffer-hook #'triage-peek--cancel-timer nil t))

(provide 'triage-peek)

;;; triage-peek.el ends here
