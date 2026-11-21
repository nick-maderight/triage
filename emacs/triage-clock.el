;;; triage-clock.el --- Clock an Org heading during Triage review -*- lexical-binding: t; -*-

;;; Commentary:

;; `triage-clock-mode' clocks the Org heading in `triage-clock-heading'
;; while a Triage review buffer is on screen in a focused frame.  A timer
;; checks every five seconds, so a glance shorter than that may not count.

;;; Code:

(require 'org-clock)

(defvar triage-launch--kind)

(defcustom triage-clock-heading nil
  "Org heading that `triage-clock-mode' clocks, as (FILE HEADING).
FILE is the Org file.  HEADING is the exact heading text without its TODO
keyword, priority or tags."
  :type '(choice (const :tag "None" nil)
                 (list (file :tag "Org file") (string :tag "Heading")))
  :group 'triage)

(defconst triage-clock--poll 5
  "Seconds between two checks for review.")

(defconst triage-clock--idle 300
  "Seconds without input after which review counts as stopped.")

(defvar triage-clock--timer nil
  "Repeating timer that runs `triage-clock--sync'.")

(defvar triage-clock--reviewing nil
  "Non-nil when the last check found review under way.")

(defvar triage-clock--resume nil
  "Marker on the heading whose clock review interrupted, or nil.")

(defun triage-clock--review-buffer-p (buffer)
  "Return non-nil when BUFFER displays Triage review.
Review buffers are the inbox, a card's context, peek or Dispatch sheet, and
the history."
  (with-current-buffer buffer
    (or (derived-mode-p 'triage-inbox-mode 'triage-context-mode
                        'triage-peek-mode 'triage-history-mode)
        (and (derived-mode-p 'triage-launch-mode)
             (eq triage-launch--kind 'dispatch)))))

(defun triage-clock--reviewing-p ()
  "Return t when review is under way, else nil.
The selected window, or the one a minibuffer prompt came from, must display a
review buffer.  Its frame must not lose focus, and Emacs must have seen
input within `triage-clock--idle' seconds."
  (let ((idle (current-idle-time))
        (frame (selected-frame)))
    (and (frame-focus-state frame)
         (< (if idle (float-time idle) 0) triage-clock--idle)
         (triage-clock--review-buffer-p
          (window-buffer (or (minibuffer-selected-window)
                             (frame-selected-window frame))))
         t)))

(defun triage-clock--ours-p ()
  "Return non-nil when the running clock is on `triage-clock-heading'."
  (and triage-clock-heading
       (org-clocking-p)
       (equal org-clock-current-task (cadr triage-clock-heading))
       (let ((marker org-clock-hd-marker)
             (file (car triage-clock-heading)))
         (and (markerp marker)
              (marker-buffer marker)
              (with-current-buffer (marker-buffer marker)
                (equal (and buffer-file-name (file-truename buffer-file-name))
                       (file-truename file)))))))

(defun triage-clock--clock-in (marker)
  "Clock in the heading at MARKER, ignoring any narrowing."
  (with-current-buffer (marker-buffer marker)
    (org-with-wide-buffer
     (goto-char marker)
     (org-clock-in))))

(defun triage-clock--start ()
  "Clock in `triage-clock-heading' and remember the clock it interrupts."
  (unless (triage-clock--ours-p)
    (let* ((file (car triage-clock-heading))
           (heading (cadr triage-clock-heading))
           (marker (and (file-exists-p file)
                        (org-find-exact-headline-in-buffer
                         heading (or (find-buffer-visiting file)
                                     (find-file-noselect file))))))
      (unless marker
        (user-error "Heading %S not found in %s" heading file))
      (when (org-clocking-p)
        (setq triage-clock--resume (copy-marker org-clock-hd-marker)))
      (unwind-protect (triage-clock--clock-in marker)
        (set-marker marker nil)))))

(defun triage-clock--stop (resume)
  "Clock out `triage-clock-heading' while its clock is running.
After an idle stop the clock ends at the last input and nothing resumes.
Otherwise, with RESUME non-nil, clock back in the task review interrupted."
  (let* ((task triage-clock--resume)
         (idle (current-idle-time))
         (away (and idle (>= (float-time idle) triage-clock--idle))))
    (setq triage-clock--resume nil)
    (when (triage-clock--ours-p)
      (org-clock-out nil t (and away
                                (let* ((last (time-subtract nil idle))
                                       (start org-clock-start-time))
                                  (if (time-less-p last start) start last))))
      (when (and resume (not away) task (marker-buffer task))
        (triage-clock--clock-in task)))
    (when task
      (set-marker task nil))))

(defun triage-clock--quietly (function &rest args)
  "Call FUNCTION with ARGS without Org messages, prompt buffers or notes.
Clocks shorter than a minute leave no line."
  (let ((inhibit-message t)
        (org-log-note-clock-out nil)
        (org-clock-out-remove-zero-time-clocks t)
        (org-clock-auto-clock-resolution nil))
    (apply function args)))

(defun triage-clock--sync ()
  "Clock review in or out when it started or stopped since the last check."
  (let ((reviewing (triage-clock--reviewing-p)))
    (unless (eq reviewing triage-clock--reviewing)
      (setq triage-clock--reviewing reviewing)
      (condition-case err
          (if reviewing
              (triage-clock--quietly #'triage-clock--start)
            (triage-clock--quietly #'triage-clock--stop t))
        (error (message "Triage review clock: %s"
                        (error-message-string err)))))))

;;;###autoload
(define-minor-mode triage-clock-mode
  "Clock `triage-clock-heading' while you review Triage cards.
Review is under way while the selected window of a focused frame shows the
inbox, a card's context, peek or Dispatch sheet, or the history, and Emacs
has seen input in the last five minutes.  A check every five seconds starts
and stops the clock.  A clock that review interrupted starts again after
review, unless review ended by going idle.  An idle stop ends the clock at
the last input."
  :global t
  :group 'triage
  (when triage-clock--timer
    (cancel-timer triage-clock--timer)
    (setq triage-clock--timer nil))
  (setq triage-clock--reviewing nil)
  (cond ((not triage-clock-mode)
         (triage-clock--quietly #'triage-clock--stop nil))
        ((not triage-clock-heading)
         (setq triage-clock-mode nil)
         (user-error "Set `triage-clock-heading' first"))
        (t
         (setq triage-clock--timer
               (run-with-timer triage-clock--poll triage-clock--poll
                               #'triage-clock--sync)))))

(provide 'triage-clock)
;;; triage-clock.el ends here
