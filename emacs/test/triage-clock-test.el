;;; triage-clock-test.el --- ERT tests for the review clock -*- lexical-binding: t; -*-

;;; Commentary:

;; Tests for `triage-clock.el'.  Each test clocks headings in a temporary
;; Org file and shows Triage buffers in the batch frame's window.

(require 'cl-lib)
(require 'ert)
(require 'triage-clock)
(require 'triage-inbox)
(require 'triage-launch)

;;; Code:

(defvar triage-clock-test--idle nil)

(defun triage-clock-test--focus (&rest _args)
  "Treat the batch frame as focused."
  t)

(defun triage-clock-test--idle-time ()
  "Return the idle time set by the current test."
  triage-clock-test--idle)
(defun triage-clock-test--run (body)
  "Call BODY with an Org buffer, an inbox buffer and another buffer.
The Org file holds a task heading above the review heading.  The frame
counts as focused."
  (let* ((file (make-temp-file "triage-clock-" nil ".org"
                               "* Task\n* triage review\n* Later\n"))
         (org (find-file-noselect file))
         (inbox (generate-new-buffer " *triage-clock-inbox*"))
         (other (generate-new-buffer " *triage-clock-other*"))
         (triage-clock-heading (list file "triage review"))
         (triage-clock--reviewing nil)
         (triage-clock--resume nil)
         (triage-clock-test--idle nil))
    (unwind-protect
        (progn
          (advice-add 'frame-focus-state :override #'triage-clock-test--focus)
          (advice-add 'current-idle-time :override #'triage-clock-test--idle-time)
          (with-current-buffer inbox (triage-inbox-mode))
          (funcall body org inbox other))
      (advice-remove 'frame-focus-state #'triage-clock-test--focus)
      (advice-remove 'current-idle-time #'triage-clock-test--idle-time)
      (when (org-clocking-p) (org-clock-out nil t))
      (dolist (buffer (list org inbox other))
        (with-current-buffer buffer (set-buffer-modified-p nil))
        (kill-buffer buffer))
      (delete-file file))))

(ert-deftest triage-clock-test-review-interrupts-and-resumes-a-task ()
  "Review takes the clock from a running task, which gets it back after."
  (triage-clock-test--run
   (lambda (org inbox other)
     (with-current-buffer org (goto-char (point-min)) (org-clock-in))
     (set-window-buffer (selected-window) inbox)
     (triage-clock--sync)
     (should (equal org-clock-current-task "triage review"))
     (set-window-buffer (selected-window) other)
     (triage-clock--sync)
     (should (equal org-clock-current-task "Task")))))

(ert-deftest triage-clock-test-review-needs-focus-input-and-a-card-screen ()
  "Only a focused, recently used card screen counts as review."
  (triage-clock-test--run
   (lambda (_org inbox other)
     (set-window-buffer (selected-window) inbox)
     (should (triage-clock--reviewing-p))
     (cl-letf (((symbol-function 'frame-focus-state) #'ignore))
       (should-not (triage-clock--reviewing-p)))
     (cl-letf (((symbol-function 'current-idle-time)
                (lambda () (seconds-to-time 600))))
       (should-not (triage-clock--reviewing-p)))
     (with-current-buffer other
       (triage-launch-mode)
       (setq triage-launch--kind 'compose))
     (set-window-buffer (selected-window) other)
     (should-not (triage-clock--reviewing-p))
     (with-current-buffer other (setq triage-launch--kind 'dispatch))
     (should (triage-clock--reviewing-p)))))

(ert-deftest triage-clock-test-idle-stop-closes-review-and-keeps-task-stopped ()
  "An idle stop closes the review clock and leaves the interrupted task off."
  (triage-clock-test--run
   (lambda (org inbox _other)
     (with-current-buffer org (goto-char (point-min)) (org-clock-in))
     (set-window-buffer (selected-window) inbox)
     (triage-clock--sync)
     (setq triage-clock-test--idle (seconds-to-time 600))
     (triage-clock--sync)
     (setq triage-clock-test--idle nil)
     (should-not (org-clocking-p))
     (with-current-buffer org
       (goto-char (point-min))
       (should-not (re-search-forward "^[ \t]*CLOCK: \\[[^]]*\\][ \t]*$" nil t))))))

(provide 'triage-clock-test)
;;; triage-clock-test.el ends here
