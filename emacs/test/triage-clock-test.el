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

(defun triage-clock-test--at (offset function)
  "Call FUNCTION as if Org's time were OFFSET seconds from now."
  (cl-letf (((symbol-function 'org-current-time)
             (lambda (&rest _args) (time-add nil offset))))
    (funcall function)))

(defun triage-clock-test--clock-line (org heading open)
  "Return the first CLOCK line under HEADING in the ORG buffer.
With OPEN non-nil, return the first clock that is still running."
  (with-current-buffer org
    (org-with-wide-buffer
     (goto-char (point-min))
     (re-search-forward (concat "^\\* " (regexp-quote heading) "$"))
     (re-search-forward (if open
                            "^[ \t]*CLOCK: \\(\\[[^]]*\\]\\)[ \t]*$"
                          "^[ \t]*CLOCK: \\[[^]]*\\]--\\(\\[[^]]*\\]\\) => +\\([0-9]+:[0-9]+\\)$"))
     (list (match-string-no-properties 1) (match-string-no-properties 2)))))

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
         (triage-clock--seen nil)
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

(ert-deftest triage-clock-test-a-minute-away-stops-review-where-it-was-seen ()
  "After a minute away review stops where it was last seen.
The task review interrupted starts again at that same time."
  (triage-clock-test--run
   (lambda (org inbox other)
     (triage-clock-test--at
      -900 (lambda () (with-current-buffer org (goto-char (point-min)) (org-clock-in))))
     (set-window-buffer (selected-window) inbox)
     (triage-clock-test--at -600 #'triage-clock--sync)
     (should (equal org-clock-current-task "triage review"))
     (set-window-buffer (selected-window) other)
     (triage-clock--sync)
     (should (equal org-clock-current-task "triage review"))
     (setq triage-clock--seen (- (float-time) 300))
     (triage-clock--sync)
     (should (equal org-clock-current-task "Task"))
     (let ((review (triage-clock-test--clock-line org "triage review" nil)))
       (should (equal (nth 1 review) "0:05"))
       (should (equal (car (triage-clock-test--clock-line org "Task" t))
                      (car review)))))))

(ert-deftest triage-clock-test-triage-screens-and-short-visits-keep-one-clock ()
  "Other Triage screens, a short visit elsewhere and lost focus keep the clock."
  (let ((sheet (generate-new-buffer "*triage new session*")))
    (unwind-protect
        (triage-clock-test--run
         (lambda (org inbox other)
           (with-current-buffer sheet
             (triage-launch-mode)
             (setq triage-launch--kind 'compose))
           (set-window-buffer (selected-window) inbox)
           (triage-clock--sync)
           (dolist (buffer (list sheet other inbox))
             (set-window-buffer (selected-window) buffer)
             (triage-clock--sync)
             (should (triage-clock--ours-p)))
           (cl-letf (((symbol-function 'frame-focus-state) #'ignore))
             (triage-clock--sync))
           (should (triage-clock--ours-p))
           (with-current-buffer org
             (should (= (count-matches "CLOCK:" (point-min) (point-max)) 1)))))
      (kill-buffer sheet))))

(ert-deftest triage-clock-test-review-starts-on-a-focused-review-screen ()
  "Review starts only on a focused, recently used review screen."
  (triage-clock-test--run
   (lambda (_org inbox other)
     (set-window-buffer (selected-window) inbox)
     (cl-letf (((symbol-function 'frame-focus-state) #'ignore))
       (triage-clock--sync))
     (should-not (org-clocking-p))
     (setq triage-clock-test--idle (seconds-to-time 600))
     (triage-clock--sync)
     (setq triage-clock-test--idle nil)
     (should-not (org-clocking-p))
     (with-current-buffer other
       (triage-launch-mode)
       (setq triage-launch--kind 'compose))
     (set-window-buffer (selected-window) other)
     (triage-clock--sync)
     (should-not (org-clocking-p))
     (with-current-buffer other (setq triage-launch--kind 'dispatch))
     (triage-clock--sync)
     (should (triage-clock--ours-p)))))

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
