;;; triage-settings.el --- Dictation dictionary and phrases for triage -*- lexical-binding: t; -*-

;;; Commentary:

;; One buffer edits the dictation dictionary and the saved phrases.  The
;; two sections are split by read-only separator lines.

;;; Code:

(require 'text-property-search)
(require 'triage-api)
(require 'triage-render)

(defun triage-settings--validate (dictionary)
  "Signal a user error for a DICTIONARY line not written heard = written."
  (let ((number 0))
    (dolist (line (split-string dictionary "\n"))
      (setq number (1+ number))
      (unless (string-empty-p (string-trim line))
        (let* ((at (string-search "=" line))
               (heard (and at (string-trim (substring line 0 at))))
               (written (and at (string-trim (substring line (1+ at))))))
          (when (or (null at) (string-empty-p heard) (string-empty-p written))
            (user-error "Please write dictionary line %d as heard = written"
                        number)))))))

(defun triage-settings--fixed (text face part)
  "Insert TEXT as a read-only line in FACE, tagged with PART when non-nil."
  (let ((start (point)))
    (insert text "\n")
    (add-text-properties
     start (point)
     `(read-only t front-sticky (read-only) rear-nonsticky (read-only)
       font-lock-face ,face triage-settings-part ,part))))

(defun triage-settings--insert (dictionary phrases)
  "Fill the current buffer with DICTIONARY and PHRASES, point in the dictionary."
  (let ((inhibit-read-only t)
        (dictionary-start nil))
    (erase-buffer)
    (triage-settings--fixed
     "Dictation settings · C-c C-c save · C-c C-k cancel" 'triage-title nil)
    (triage-settings--fixed
     "──── Dictionary: one \"heard = written\" per line ────"
     'triage-muted 'dictionary)
    (setq dictionary-start (point))
    (insert dictionary)
    (unless (or (string-empty-p dictionary) (string-suffix-p "\n" dictionary))
      (insert "\n"))
    (triage-settings--fixed
     "──── Phrases: one per line, offered by C-c C-o p in launch buffers ────"
     'triage-muted 'phrases)
    (insert phrases)
    (goto-char dictionary-start)))

(defun triage-settings--sections ()
  "Return (DICTIONARY . PHRASES), the text after each separator line."
  (save-excursion
    (goto-char (point-min))
    (let ((dictionary (text-property-search-forward
                       'triage-settings-part 'dictionary #'eq)))
      (goto-char (point-min))
      (let ((phrases (text-property-search-forward
                      'triage-settings-part 'phrases #'eq)))
        (unless (and dictionary phrases)
          (user-error "The dictation separators are missing"))
        (cons (buffer-substring-no-properties
               (prop-match-end dictionary) (prop-match-beginning phrases))
              (buffer-substring-no-properties
               (prop-match-end phrases) (point-max)))))))

(defun triage-settings-save ()
  "Validate the dictionary, then save the dictionary and phrases."
  (interactive)
  (pcase-let ((`(,dictionary . ,phrases) (triage-settings--sections)))
    (triage-settings--validate dictionary)
    (condition-case err
        (progn
          (triage-api-request-sync
           'post "/v1/dictation"
           :body `((dictionary . ,dictionary) (phrases . ,phrases)))
          (message "Saved."))
      (triage-api-error
       (message "%s" (propertize (format "Save failed: %s" (nth 2 err))
                                 'face 'error))))))

(defun triage-settings-abort ()
  "Discard the settings buffer."
  (interactive)
  (kill-buffer (current-buffer)))

(defvar-keymap triage-settings-mode-map
  "C-c C-c" #'triage-settings-save
  "C-c C-k" #'triage-settings-abort)

(define-minor-mode triage-settings-mode
  "Bind the save and abort keys of the triage settings buffer."
  :keymap triage-settings-mode-map)

;;;###autoload
(defun triage-settings ()
  "Edit the dictation dictionary and the saved phrases."
  (interactive)
  (let ((data (condition-case err
                  (triage-api-request-sync 'get "/v1/dictation")
                (triage-api-error
                 (user-error "Could not load dictation settings (%s)"
                             (nth 2 err)))))
        (buffer (get-buffer-create "*triage settings*")))
    (with-current-buffer buffer
      (text-mode)
      (triage-settings-mode 1)
      (triage-settings--insert (or (alist-get 'dictionary data) "")
                               (or (alist-get 'phrases data) "")))
    (pop-to-buffer buffer)))

(provide 'triage-settings)

;;; triage-settings.el ends here
