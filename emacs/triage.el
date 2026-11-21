;;; triage.el --- Emacs client for the Triage decision inbox -*- lexical-binding: t; -*-

;; Author: Nick <nick@maderightsoftware.com>
;; Version: 0.1.0
;; Package-Requires: ((emacs "30.1") (plz "0.9") (magit-section "4.0") (transient "0.7"))
;; Keywords: tools, convenience
;; SPDX-License-Identifier: MIT
;; Homepage: https://github.com/nick-maderight/triage

;;; Commentary:
;;
;; Open the live Triage decision inbox and work cards from Emacs.
;;
;;   M-x triage             open the inbox
;;   M-x triage-new-session start a session
;;   M-x triage-history     browse handled cards
;;   M-x triage-settings    edit dictation phrases
;;   M-x triage-delegate   dispatch the current Org, mail, Dired or region context
;;   M-x triage-mode-line-mode toggle the global counter
;;
;; In the inbox, use RET for the primary action, 1-9 for options, j/k to move,
;; d to dispatch, s to skip, l to defer, t for context, v to peek and ? for keys.
;;
;;; Code:

(require 'triage-api)
(require 'triage-card)
(require 'triage-queue)
(require 'triage-render)
(require 'triage-launch)
(require 'triage-context)
(require 'triage-peek)
(require 'triage-history)
(require 'triage-settings)
(require 'triage-inbox)

;;;###autoload
(defun triage ()
  "Open the live Triage inbox."
  (interactive)
  (let ((buffer (get-buffer-create "*triage*")))
    (with-current-buffer buffer
      (unless (derived-mode-p 'triage-inbox-mode)
        (triage-inbox-mode))
      (add-hook 'kill-buffer-hook #'triage--inbox-killed nil t)
      (add-hook 'triage-queue-update-hook #'triage-inbox--on-update)
      (triage-queue-subscribe 'inbox)
      (triage-inbox--render))
    (pop-to-buffer-same-window buffer)))

(defun triage--inbox-killed ()
  "Stop the inbox queue consumer when its buffer closes."
  (remove-hook 'triage-queue-update-hook #'triage-inbox--on-update)
  (triage-queue-unsubscribe 'inbox))

(defun triage-mode-line--string ()
  "Return the queue count for the global mode line."
  (let ((counts triage-queue-counts))
    (if (eq triage-queue-connection 'reconnecting)
        (propertize " triage:?" 'face 'triage-muted)
      (let ((total (apply #'+ (mapcar (lambda (kind)
                                       (or (alist-get kind counts) 0))
                                     '(ask plan finding info)))))
        (if (zerop total)
            ""
          (propertize (format " triage:%d" total)
                      'face (if (> (+ (or (alist-get 'ask counts) 0)
                                      (or (alist-get 'plan counts) 0))
                                   0)
                                'triage-status-warning
                              'triage-muted)
                      'help-echo (format "ask %d · plan %d · finding %d · info %d\nmouse-1: open the inbox"
                                         (or (alist-get 'ask counts) 0)
                                         (or (alist-get 'plan counts) 0)
                                         (or (alist-get 'finding counts) 0)
                                         (or (alist-get 'info counts) 0))
                      'local-map (let ((map (make-sparse-keymap)))
                                   (define-key map [mode-line mouse-1] #'triage)
                                   map)))))))

;;;###autoload
(define-minor-mode triage-mode-line-mode
  "Show the Triage queue count in the global mode line."
  :group 'triage
  (if triage-mode-line-mode
      (progn
        (unless global-mode-string
          (setq global-mode-string '("")))
        (add-to-list 'global-mode-string '(:eval (triage-mode-line--string)) t)
        (add-hook 'triage-queue-update-hook #'force-mode-line-update)
        (triage-queue-subscribe 'mode-line))
    (setq global-mode-string
          (delete '(:eval (triage-mode-line--string)) global-mode-string))
    (remove-hook 'triage-queue-update-hook #'force-mode-line-update)
    (triage-queue-unsubscribe 'mode-line)))

(provide 'triage)
;;; triage.el ends here
