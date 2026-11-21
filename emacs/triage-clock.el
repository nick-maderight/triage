;;; triage-clock.el --- Clock an Org heading during Triage review -*- lexical-binding: t; -*-

;;; Commentary:

;; `triage-clock-mode' clocks the Org heading in `triage-clock-heading'
;; while you review Triage cards.  A timer checks every five seconds, so a
;; glance shorter than that may not count.  Moving between Triage screens
;; keeps the clock running, as does a visit elsewhere shorter than a
;; minute.

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

(defconst triage-clock--grace 60
  "Seconds away from Triage after which review counts as stopped.")

(defconst triage-clock--idle 300
  "Seconds without input after which review counts as stopped.")

(defvar triage-clock--timer nil
  "Repeating timer that runs `triage-clock--sync'.")

(defvar triage-clock--seen nil
  "Float time when a check last found review under way, or nil.
Nil means review is not under way.")

(defvar triage-clock--resume nil
  "Marker on the heading whose clock review interrupted, or nil.")

(defun triage-clock--review-buffer-p (buffer)
  "Return non-nil when review can start in BUFFER.
Review buffers are the inbox, a card's context, peek or Dispatch sheet, and
the history."
  (with-current-buffer buffer
    (or (derived-mode-p 'triage-inbox-mode 'triage-context-mode
                        'triage-peek-mode 'triage-history-mode)
        (and (derived-mode-p 'triage-launch-mode)
             (eq triage-launch--kind 'dispatch)))))

(defun triage-clock--triage-buffer-p (buffer)
  "Return non-nil when BUFFER is any Triage screen.
Review under way continues on such a screen.  Every Triage buffer name
starts with `*triage'."
  (string-prefix-p "*triage" (buffer-name buffer)))

(defun triage-clock--screen ()
  "Return the buffer you are looking at, or nil while Emacs lacks focus.
That is the buffer in the selected window of a focused frame, or in the
window a minibuffer prompt came from."
  (let ((frame (selected-frame)))
    (and (frame-focus-state frame)
         (window-buffer (or (minibuffer-selected-window)
                            (frame-selected-window frame))))))

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

(defun triage-clock--clock-in (marker &optional start)
  "Clock in the heading at MARKER, ignoring any narrowing.
The clock starts at START, a Lisp time, or now when START is nil."
  (with-current-buffer (marker-buffer marker)
    (org-with-wide-buffer
     (goto-char marker)
     (org-clock-in nil start))))

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

(defun triage-clock--stop (end resume)
  "Clock out `triage-clock-heading' at END while its clock is running.
END is a float time.  An END before the clock started becomes its start.
With RESUME non-nil, the clock review interrupted starts again at END."
  (let ((task triage-clock--resume))
    (setq triage-clock--resume nil)
    (when (triage-clock--ours-p)
      (let ((end (if (< end (float-time org-clock-start-time))
                     org-clock-start-time
                   (seconds-to-time end))))
        (org-clock-out nil t end)
        (when (and resume task (marker-buffer task))
          (triage-clock--clock-in task end))))
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
  "Start, keep or stop the review clock.
Review starts on a review screen and any Triage screen keeps it going.
After `triage-clock--grace' seconds elsewhere it stops where a check last
saw it.  The clock it interrupted then starts again from there.  After
`triage-clock--idle' seconds without input on a Triage screen it stops at
the last input."
  (let* ((now (float-time))
         (idle (float-time (or (current-idle-time) 0)))
         (seen triage-clock--seen)
         (buffer (triage-clock--screen))
         (here (and buffer
                    (or (triage-clock--review-buffer-p buffer)
                        (and seen (triage-clock--triage-buffer-p buffer))))))
    (condition-case err
        (cond ((not here)
               (when (and seen (>= (- now seen) triage-clock--grace))
                 (setq triage-clock--seen nil)
                 (triage-clock--quietly #'triage-clock--stop seen t)))
              ((>= idle triage-clock--idle)
               (when seen
                 (setq triage-clock--seen nil)
                 (triage-clock--quietly #'triage-clock--stop (- now idle) nil)))
              (t
               (setq triage-clock--seen now)
               (unless seen
                 (triage-clock--quietly #'triage-clock--start))))
      (error (message "Triage review clock: %s" (error-message-string err))))))

;;;###autoload
(define-minor-mode triage-clock-mode
  "Clock `triage-clock-heading' while you review Triage cards.
Review starts when the selected window of a focused frame shows the inbox,
a card's context, peek or Dispatch sheet, or the history, and Emacs has
seen input in the last five minutes.  Every Triage screen keeps it going,
as does a visit elsewhere shorter than a minute.  After a minute away the
clock stops where review was last seen.  A clock that review interrupted
then starts again from there.  Five minutes without input on a Triage
screen stop the clock at the last input and leave the interrupted clock
stopped.  A check every five seconds starts and stops the clock."
  :global t
  :group 'triage
  (when triage-clock--timer
    (cancel-timer triage-clock--timer)
    (setq triage-clock--timer nil))
  (let ((seen triage-clock--seen))
    (setq triage-clock--seen nil)
    (cond ((not triage-clock-mode)
           (triage-clock--quietly #'triage-clock--stop (or seen (float-time)) nil))
          ((not triage-clock-heading)
           (setq triage-clock-mode nil)
           (user-error "Set `triage-clock-heading' first"))
          (t
           (setq triage-clock--timer
                 (run-with-timer triage-clock--poll triage-clock--poll
                                 #'triage-clock--sync))))))

(provide 'triage-clock)
;;; triage-clock.el ends here
