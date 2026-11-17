;;; triage-history.el --- Handled-card history and context for triage -*- lexical-binding: t; -*-

;;; Commentary:

;; The history table lists cards handled recently.  The context view shows
;; the thread around one card.  Both read the swipe records the daemon keeps.

;;; Code:

(require 'cl-lib)
(require 'magit-section)
(require 'vtable)
(require 'triage-api)
(require 'triage-card)
(require 'triage-queue)
(require 'triage-render)

(defun triage-history--midnight-ms (days)
  "Return local midnight DAYS after today, in epoch milliseconds."
  (let* ((now (decode-time))
         (midnight (encode-time
                    (list 0 0 0 (+ (decoded-time-day now) days)
                          (decoded-time-month now) (decoded-time-year now)
                          nil -1 nil))))
    (truncate (* 1000 (float-time midnight)))))

(defun triage-history-fetch (then else)
  "Fetch the recent history and call THEN with the decoded body.
ELSE receives the failure as (STATUS MESSAGE), as `triage-api-request'
does.  The body holds `cards' and `today_count'."
  (triage-api-request
   'get
   (format "/v1/history?limit=50&today_start_ms=%d&today_end_ms=%d"
           (triage-history--midnight-ms 0)
           (triage-history--midnight-ms 1))
   :then then
   :else else))

(defun triage-history--tone-face (tone)
  "Return the face for status TONE."
  (pcase tone
    ('positive 'triage-status-positive)
    ('warning 'triage-status-warning)
    ('negative 'triage-status-negative)
    (_ 'triage-status-neutral)))

(defun triage-history--title (card)
  "Return the title of CARD, or `Untitled card' when it has none."
  (let ((title (alist-get 'title card)))
    (if (and (stringp title) (not (string-empty-p title)))
        title
      "Untitled card")))

(defun triage-history--columns (now)
  "Return the history table columns, with clocks read at NOW."
  (list
   (list :name "Time" :width 9 :align 'left
         :getter (lambda (row _table)
                   (let ((at (alist-get 'at_ms (alist-get 'swipe row))))
                     (if at (triage-card-clock at now) "Time unknown"))))
   (list :name "Kind" :width 9 :align 'left
         :getter (lambda (row _table)
                   (or (alist-get 'kind (alist-get 'card row)) "unknown")))
   (list :name "Status" :width 18 :align 'left
         :getter (lambda (row _table)
                   (let ((status (triage-card-status (alist-get 'card row))))
                     (propertize (plist-get status :label)
                                 'font-lock-face
                                 (triage-history--tone-face
                                  (plist-get status :tone))))))
   (list :name "Source" :width 16 :align 'left
         :getter (lambda (row _table)
                   (triage-card-source-name
                    (alist-get 'source (alist-get 'card row)))))
   (list :name "Title" :width 40 :align 'left
         :getter (lambda (row _table)
                   (triage-history--title (alist-get 'card row))))
   (list :name "Result" :width 40 :align 'left
         :getter (lambda (row _table)
                   (let* ((card (alist-get 'card row))
                          (swipe (alist-get 'swipe row))
                          (details (triage-card-swipe-details card swipe)))
                     (concat (triage-card-swipe-status swipe "unknown")
                             (if (and details (not (string-empty-p details)))
                                 (concat " · " details)
                               "")))))))

(defvar-local triage-history--today nil
  "Count of cards handled today, or nil until the first load.")

(defvar-keymap triage-history-mode-map
  :parent special-mode-map
  "RET" #'triage-history-context
  "t" #'triage-history-context
  "r" #'triage-history-reload
  "g" #'triage-history-refresh
  "q" #'quit-window)

(define-derived-mode triage-history-mode special-mode "Triage-History"
  "Major mode that lists cards handled recently."
  (setq-local header-line-format '(:eval (triage-history--header-line))))

(defun triage-history--header-line ()
  "Return the header line of the history buffer."
  (format "History · Handled today: %s · RET context · r reload · g refresh · q quit"
          (or triage-history--today "…")))

(defun triage-history--show-text (buffer text)
  "Replace the contents of history BUFFER with TEXT, when BUFFER is live."
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (let ((inhibit-read-only t))
        (erase-buffer)
        (insert text "\n")
        (goto-char (point-min))))))

(defun triage-history--show-body (buffer body)
  "Show the history BODY in BUFFER as a table, or its empty note."
  (when (buffer-live-p buffer)
    (with-current-buffer buffer
      (let ((inhibit-read-only t)
            (rows (alist-get 'cards body)))
        (setq triage-history--today (alist-get 'today_count body))
        (erase-buffer)
        (if rows
            (make-vtable :columns (triage-history--columns (float-time))
                         :objects rows
                         :use-header-line nil
                         :keymap triage-history-mode-map)
          (insert "No handled cards in recent history.\n"))
        (goto-char (point-min))))))

(defun triage-history--load (buffer)
  "Fetch the history into BUFFER and show it there."
  (triage-history--show-text buffer "Loading history…")
  (triage-history-fetch
   (lambda (body) (triage-history--show-body buffer body))
   (lambda (_status msg)
     (triage-history--show-text
      buffer (format "History could not be loaded: %s" msg)))))

(defun triage-history--current ()
  "Return the history row at point, refusing a deferred card."
  (let ((row (or (vtable-current-object)
                 (user-error "No history row at point"))))
    (when (equal (alist-get 'action (alist-get 'swipe row)) "later")
      (user-error "A deferred card returns by itself"))
    row))

(defun triage-history-context ()
  "Open the context thread of the card on the current row."
  (interactive)
  (triage-context-open
   (alist-get 'id (alist-get 'card (triage-history--current)))))

(defun triage-history-reload ()
  "Put the card on the current row back into the queue."
  (interactive)
  (let* ((card (alist-get 'card (triage-history--current)))
         (name (triage-history--title card))
         (buffer (current-buffer)))
    (triage-api-request
     'post (triage-api-card-path (alist-get 'id card) "reload")
     :then (lambda (_)
             (message "Reloaded · %s" name)
             (triage-queue-refresh)
             (triage-history--load buffer))
     :else (lambda (status msg)
             (if (eql status 409)
                 (message "Already queued · %s" name)
               (message "%s"
                        (propertize (format "Reload failed: %s" msg)
                                    'face 'error)))))))

(defun triage-history-refresh ()
  "Fetch the history again."
  (interactive)
  (triage-history--load (current-buffer)))

;;;###autoload
(defun triage-history ()
  "Show the cards handled today and in recent history."
  (interactive)
  (let ((buffer (get-buffer-create "*triage history*")))
    (with-current-buffer buffer
      (unless (derived-mode-p 'triage-history-mode)
        (triage-history-mode)))
    (pop-to-buffer buffer)
    (triage-history--load buffer)))

(defvar-local triage-context--id nil
  "Id of the card whose thread this buffer shows.")

(defvar-local triage-context--token 0
  "Count of loads, so an answer to an earlier load is ignored.")

(defvar-keymap triage-context-mode-map
  :parent magit-section-mode-map
  "g" #'triage-context-refresh
  "q" #'quit-window)

(define-derived-mode triage-context-mode magit-section-mode "Triage-Context"
  "Major mode that shows the thread of cards around one card.")

(defun triage-context--show-text (buffer text)
  "Replace the contents of context BUFFER with TEXT."
  (with-current-buffer buffer
    (let ((inhibit-read-only t))
      (erase-buffer)
      (insert text "\n")
      (goto-char (point-min)))))

(defun triage-context--show-body (buffer body)
  "Show the context BODY in BUFFER and point at the anchor card."
  (with-current-buffer buffer
    (let ((inhibit-read-only t)
          (items (alist-get 'items body))
          (anchor (alist-get 'anchor body))
          (truncated (alist-get 'truncated body))
          (anchor-at nil)
          (previous nil))
      (erase-buffer)
      (when (and (integerp truncated) (> truncated 0))
        (insert (propertize (format "%d older cards not shown.\n" truncated)
                            'font-lock-face 'triage-muted)))
      (if (null items)
          (insert "No cards in this thread.\n")
        (dolist (item items)
          (let* ((card (alist-get 'card item))
                 (ref (car (alist-get 'refs card)))
                 (anchor-p (equal (alist-get 'id card) anchor)))
            (when anchor-p
              (setq anchor-at (point)))
            (triage-render-context-item
             card (alist-get 'swipes item) anchor-p
             (and (stringp ref) (not (string-empty-p ref))
                  (equal ref previous)))
            (setq previous ref))))
      (goto-char (or anchor-at (point-min))))))

(defun triage-context--load (buffer id)
  "Fetch the thread of card ID into BUFFER."
  (let ((token (with-current-buffer buffer
                 (setq triage-context--token (1+ triage-context--token)))))
    (triage-context--show-text buffer "Loading context…")
    (cl-flet ((answer (fn)
                (when (and (buffer-live-p buffer)
                           (eql token (buffer-local-value
                                       'triage-context--token buffer)))
                  (funcall fn))))
      (triage-api-request
       'get (triage-api-card-path id "context")
       :then (lambda (body)
               (answer (lambda ()
                         (triage-context--show-body buffer body))))
       :else (lambda (_status msg)
               (answer (lambda ()
                         (triage-context--show-text
                          buffer
                          (format "Context could not be loaded: %s" msg)))))))))

(defun triage-context-refresh ()
  "Fetch the context thread again."
  (interactive)
  (triage-context--load (current-buffer) triage-context--id))

(defun triage-context-open (id)
  "Open the thread of cards around card ID in the context buffer."
  (let ((buffer (get-buffer-create "*triage context*")))
    (with-current-buffer buffer
      (unless (derived-mode-p 'triage-context-mode)
        (triage-context-mode))
      (setq triage-context--id id))
    (pop-to-buffer buffer)
    (triage-context--load buffer id)))

(provide 'triage-history)

;;; triage-history.el ends here
