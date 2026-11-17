;;; triage-api.el --- Daemon transport for the triage client -*- lexical-binding: t; -*-

;;; Commentary:

;; HTTP and SSE transport for the triage daemon.  Requests go through
;; plz, as `redsheet--request' does.  This file knows nothing about
;; cards or views.  The other triage-* files build on it.

;;; Code:

(require 'cl-lib)
(require 'json)
(require 'plz)
(require 'subr-x)
(require 'url-util)

(defgroup triage nil
  "Emacs client for the triage decision inbox."
  :group 'tools)

(defcustom triage-url (or (getenv "TRIAGE_URL") "http://127.0.0.1:7340")
  "Base URL of the triage daemon."
  :type 'string)

(define-error 'triage-api-error "Triage request failed")

(defun triage-api-url (path)
  "Return the absolute URL for PATH, which must start with a slash."
  (concat (string-remove-suffix "/" triage-url) path))

(defun triage-api-card-path (id verb)
  "Return the card path for ID and VERB."
  (format "/v1/cards/%s/%s" (url-hexify-string id) verb))

(defun triage-api-session-path (pane suffix)
  "Return the path for PANE, ending in SUFFIX, or without it when SUFFIX is nil.
A PANE that starts with \"dsh:\" addresses a dsh session.  Any other
PANE addresses a tmux pane."
  (let* ((dsh (string-prefix-p "dsh:" pane))
         (base (format "/v1/%s/%s"
                       (if dsh "dsh" "panes")
                       (url-hexify-string (if dsh (substring pane 4) pane)))))
    (if suffix (concat base "/" suffix) base)))

(defun triage-api--encode (body)
  "Serialize BODY to the JSON bytes sent as a request body."
  (let ((json (json-serialize body :null-object :null :false-object :false)))
    (if (multibyte-string-p json)
        (encode-coding-string json 'utf-8)
      json)))

(defun triage-api--decode (string)
  "Parse the JSON in STRING into alists, lists and nil.
A blank STRING gives nil."
  (unless (or (null string) (string-empty-p (string-trim string)))
    (json-parse-string string
                       :object-type 'alist
                       :array-type 'list
                       :null-object nil
                       :false-object nil)))

(defun triage-api--field (body key)
  "Return KEY from the decoded BODY, or nil when BODY is not an object."
  (and (listp body) (alist-get key body)))

(defun triage-api--failure (err)
  "Return (STATUS . MESSAGE) for the `plz-error' ERR.
STATUS is nil when the daemon could not be reached."
  (let ((response (plz-error-response err)))
    (if response
        (let* ((status (plz-response-status response))
               (body (condition-case nil
                         (triage-api--decode (plz-response-body response))
                       (error nil))))
          (cons status
                (or (triage-api--field body 'error)
                    (triage-api--field body 'message)
                    (format "HTTP %s" status))))
      (cons nil (format "Daemon unreachable (%s)"
                        (string-trim (or (cdr (plz-error-curl-error err)) "")))))))

(defun triage-api--report-failure (_status message)
  "Show MESSAGE, the text of a failed request, in the echo area."
  (message "Triage: %s" message))

(defun triage-api--headers (post)
  "Return request headers, adding a JSON content type when POST is non-nil."
  (if post
      '(("Accept" . "application/json")
        ("Content-Type" . "application/json"))
    '(("Accept" . "application/json"))))

(cl-defun triage-api-request (method path &key body then else (timeout 120))
  "Send METHOD to PATH asynchronously and return the curl process.
METHOD is `get' or `post'.  BODY is an alist sent as JSON on a POST.
THEN receives the decoded response body.  ELSE receives the status and
message of a failure.  It defaults to showing the message.  TIMEOUT is
the limit in seconds."
  (let ((post (eq method 'post)))
    (plz method (triage-api-url path)
      :headers (triage-api--headers post)
      :body (and post (triage-api--encode (or body '())))
      :timeout timeout
      :as 'response
      :body-type 'binary
      :then (lambda (response)
              (let ((decoded nil) (decode-error nil))
                (condition-case err
                    (setq decoded (triage-api--decode (plz-response-body response)))
                  (error (setq decode-error err)))
                (if decode-error
                    (funcall (or else #'triage-api--report-failure)
                             nil
                             (format "Unexpected response: %s"
                                     (error-message-string decode-error)))
                  (funcall (or then #'ignore) decoded))))
      :else (lambda (err)
              (let ((failure (triage-api--failure err)))
                (funcall (or else #'triage-api--report-failure)
                         (car failure) (cdr failure)))))))

(cl-defun triage-api-request-sync (method path &key body (timeout 15))
  "Send METHOD to PATH synchronously and return the decoded body.
BODY and TIMEOUT are as for `triage-api-request'.  A failure signals
`triage-api-error' with data (STATUS MESSAGE)."
  (let ((post (eq method 'post)))
    (condition-case err
        (let ((response (plz method (triage-api-url path)
                          :headers (triage-api--headers post)
                          :body (and post (triage-api--encode (or body '())))
                          :timeout timeout
                          :as 'response
                          :body-type 'binary)))
          (condition-case decode-error
              (triage-api--decode (plz-response-body response))
            (error
             (signal 'triage-api-error
                     (list nil (format "Unexpected response: %s"
                                       (error-message-string decode-error)))))))
      (plz-error
       (let ((failure (nth 2 err)))
         (if (plz-error-p failure)
             (let ((parts (triage-api--failure failure)))
               (signal 'triage-api-error (list (car parts) (cdr parts))))
           (signal 'triage-api-error (list nil (error-message-string err)))))))))

(defun triage-api--parse-block (block)
  "Return (EVENT . DATA) for the SSE BLOCK.
EVENT is the value of its `event:' line.  DATA joins its `data:' lines
with newlines.  Keepalive lines that start with a colon are skipped."
  (let (event data)
    (dolist (line (split-string block "\n" t))
      (cond ((string-prefix-p ":" line))
            ((string-prefix-p "event:" line)
             (setq event (string-trim (substring line 6))))
            ((string-prefix-p "data:" line)
             (push (string-trim-left (substring line 5)) data))))
    (cons event (and data (string-join (nreverse data) "\n")))))

(defun triage-api--take-events (start)
  "Delete the complete SSE blocks after START in the current buffer.
Return the decoded data of each `queue' block, in arrival order.  Data
that does not decode gives nil."
  (let (events)
    ;; Normalize CRLF first, so block boundaries match on either line ending.
    (goto-char start)
    (while (search-forward "\r\n" nil t)
      (replace-match "\n" t t))
    (goto-char start)
    (while (search-forward "\n\n" nil t)
      (let ((block (buffer-substring-no-properties start (point))))
        (delete-region start (point))
        (goto-char start)
        (let ((event (triage-api--parse-block block)))
          (when (equal (car event) "queue")
            (push (condition-case nil
                      (triage-api--decode (cdr event))
                    (error nil))
                  events)))))
    (nreverse events)))

(defun triage-api--stream-filter (process string)
  "Append STRING from PROCESS to its buffer and dispatch queue events.
The header block stays in the buffer.  Consumed event blocks are deleted,
so a stream that stays open for days does not grow the buffer."
  (let ((buffer (process-buffer process)))
    (when (buffer-live-p buffer)
      (let ((events
             (with-current-buffer buffer
               (save-excursion
                 (goto-char (point-max))
                 (insert string))
               (unless (process-get process :triage-body)
                 (goto-char (point-min))
                 (when (search-forward "\r\n\r\n" nil t)
                   (process-put process :triage-body (point))))
               (when-let ((start (process-get process :triage-body)))
                 (triage-api--take-events start)))))
        (dolist (data events)
          (funcall (process-get process :triage-on-event) data))))))

(defun triage-api-stream (on-event on-close)
  "Open the daemon's SSE stream and return its curl process.
ON-EVENT receives the decoded data of each `queue' event.  ON-CLOSE
runs with no arguments when the stream ends."
  ;; The no-buffer flag makes curl flush small events while stdout is a pipe.
  (let* ((plz-curl-default-args (cons "--no-buffer" plz-curl-default-args))
         (process (plz 'get (triage-api-url "/v1/stream")
                    :headers '(("Accept" . "text/event-stream"))
                    :as 'string
                    :timeout nil
                    :noquery t
                    :then #'ignore
                    :else #'ignore
                    :finally on-close
                    :filter #'triage-api--stream-filter)))
    (process-put process :triage-on-event on-event)
    process))

(provide 'triage-api)
;;; triage-api.el ends here
