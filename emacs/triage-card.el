;;; triage-card.el --- Pure card model for the triage client -*- lexical-binding: t; -*-

;;; Commentary:

;; Pure functions over decoded card alists.  Each one ports a function
;; from ui/app.js, so the Emacs client shows the same status, title,
;; lead and options as the web inbox.  Nothing here does I/O.

;;; Code:

(require 'cl-lib)
(require 'seq)
(require 'subr-x)

(defconst triage-card-status-labels
  '(("complete" . ("Complete" . positive))
    ("step-open" . ("Step left open" . warning))
    ("incomplete" . ("Incomplete" . negative))
    ("no-final-report" . ("No final report" . warning))
    ("no-status" . ("No final report" . warning))
    ("reply" . ("Reply"))
    ("plan-ready" . ("Plan ready"))
    ("waiting" . ("Waiting on you"))
    ("notice" . ("Notice")))
  "Map each card status to its (LABEL . TONE).
A status without a tone is stored as (LABEL), so its tone is nil.")

(defconst triage-card-report-prefixes
  '(("Done: " . "complete")
    ("Agent done: " . "step-open")
    ("Incomplete: " . "incomplete")
    ("Unreported: " . "no-final-report")
    ("Plan ready: " . "plan-ready"))
  "Title prefixes that state a report's outcome, mapped to that status.")

(defconst triage-card--dsh-uri-prefix "dsh://session/"
  "URI prefix of a card that belongs to a dsh session.")

(defun triage-card--nonempty-p (value)
  "Return non-nil when VALUE is a non-empty string."
  (and (stringp value) (not (string-empty-p value))))

(defun triage-card--first-text (&rest values)
  "Return the first non-empty string among VALUES, or nil."
  (seq-find #'triage-card--nonempty-p values))

(defun triage-card--option-label (options index)
  "Return the label of the option at INDEX in OPTIONS, or nil."
  (and (integerp index) (alist-get 'label (nth index options))))

(defun triage-card--find-model (models selector)
  "Return the model in MODELS whose selector is SELECTOR, or nil."
  (and selector
       (cl-find selector models
                :key (lambda (model) (alist-get 'selector model))
                :test #'equal)))

(defun triage-card--pictographic-p (char)
  "Return non-nil when CHAR is a pictograph, joiner or variation selector."
  (or (memq (get-char-code-property char 'general-category) '(So Sk Cf))
      (memq char '(#xFE0F #x200D))))

(defun triage-card--strip-pictographs (text)
  "Return TEXT without its leading pictographs and the spaces after them."
  (let ((end 0))
    (while (and (< end (length text))
                (triage-card--pictographic-p (aref text end)))
      (setq end (1+ end)))
    (if (zerop end) text (string-trim-left (substring text end)))))

(defun triage-card--cwd-base (cwd)
  "Return the last path segment of CWD, ignoring trailing slashes."
  (car (last (split-string (replace-regexp-in-string "[\\/]+\\'" "" cwd)
                           "[\\/]"))))

(defun triage-card--legacy-status (card)
  "Return the status that CARD gets when it states none."
  (let ((kind (alist-get 'kind card)))
    (cond ((equal kind "info")
           (if (alist-get 'refs card) "no-final-report" "notice"))
          ((equal kind "plan") "plan-ready")
          ((member kind '("ask" "finding")) "waiting")
          (t "notice"))))

(defconst triage-card-modes
  '(("goal" "Goal" "The session works on it until it reports done.")
    ("plan" "Plan" "Read-only planning. The plan arrives here for review before anything runs.")
    ("plan-into" "Plan into"
     "The planner drafts a plan read-only, the harness approves it without review, and the execution model carries it out at the execution effort chosen below."))
  "Launch sheet modes as (WIRE LABEL HELP) rows, the ways a session starts.")

(defconst triage-card-tasks
  '(("investigate" "Investigate" "Read-only. The session checks the facts and reports back." "Investigating")
    ("fix" "Fix" "Finds the root cause, fixes it for good, and checks nothing else broke." "Fixing")
    ("act" "Act" "Carries out the work with the smallest change that fully solves it." "Acting")
    ("verify" "Verify" "Checks the work is done and right, runs the full test gate, and fixes any gap." "Verifying")
    ("simplify" "Simplify" "Makes it smaller and plainer without changing what it does." "Simplifying"))
  "This constant lists launch tasks.  Each row contains all launch text.
The row stores the wire value, label, help, and verb, while the daemon
applies its direction text.")
(defun triage-card--acting-verb (mode task)
  "Return the past-tense verb shown for a dispatch in MODE and TASK."
  (if (member mode '("plan" "plan-into"))
      "Planning"
    (or (nth 3 (assoc task triage-card-tasks)) "Launching")))

(defun triage-card--answer-label (card answer index)
  "Return the text shown for ANSWER to question INDEX of CARD."
  (or (triage-card--first-text
       (alist-get 'text answer)
       (triage-card--option-label
        (alist-get 'options (nth index (alist-get 'questions card)))
        (alist-get 'option answer)))
      "answer"))

(defun triage-card-heading (text)
  "Return the heading text of TEXT, or the empty string.
The heading is the first non-blank line, when it is a Markdown heading."
  (let ((line (cl-find-if-not #'string-empty-p
                              (mapcar #'string-trim
                                      (split-string (or text "") "\r?\n")))))
    (if (and line (string-match "^#\\{1,6\\}[ \t]+\\(.+\\)" line))
        (match-string 1 line)
      "")))

(defun triage-card-title-of (card)
  "Return the title of CARD.
An info card uses its body heading when one exists."
  (let ((title (or (alist-get 'title card) "")))
    (if (equal (alist-get 'kind card) "info")
        (let ((heading (triage-card-heading (alist-get 'body card))))
          (if (string-empty-p heading) title heading))
      title)))

(defun triage-card-status (card)
  "Return the status line of CARD as (:label LABEL :tone TONE :title TITLE).
Port of cardStatus and legacyStatus in ui/app.js."
  (let* ((title (or (alist-get 'title card) ""))
         (report (member (alist-get 'kind card) '("info" "plan")))
         (hit (and report
                   (seq-find (lambda (prefix) (string-prefix-p (car prefix) title))
                             triage-card-report-prefixes)))
         (hit-key (cdr hit))
         (legacy (triage-card--legacy-status card))
         (stated (or (alist-get 'status card) hit-key legacy))
         (key (if (assoc stated triage-card-status-labels) stated (or hit-key legacy)))
         (entry (cdr (assoc key triage-card-status-labels))))
    (list :label (car entry)
          :tone (cdr entry)
          :title (if hit
                     (let ((heading (triage-card-heading (alist-get 'body card))))
                       (if (string-empty-p heading)
                           (substring title (length (car hit)))
                         heading))
                   (triage-card-title-of card)))))

(defun triage-card-dsh-id (card)
  "Return the dsh session id of CARD, or nil when it has none."
  (let ((uri (alist-get 'uri (alist-get 'source card))))
    (when (and (equal (alist-get 'engine card) "dsh")
               (stringp uri)
               (string-prefix-p triage-card--dsh-uri-prefix uri))
      (let ((id (substring uri (length triage-card--dsh-uri-prefix))))
        (and (not (string-empty-p id)) id)))))

(defun triage-card-pane (card)
  "Return the pane that CARD came from, or nil.
A dsh card names its pane as dsh:ID."
  (or (alist-get 'pane (alist-get 'source card))
      (let ((id (triage-card-dsh-id card)))
        (and id (concat "dsh:" id)))))

(defun triage-card-answerable-p (card)
  "Return non-nil when CARD can be answered in this client.
Ask and plan cards carry an explicit answerable flag."
  (if (member (alist-get 'kind card) '("ask" "plan"))
      (alist-get 'answerable card t)
    t))

(defun triage-card-restorable-p (card)
  "Return non-nil when CARD is an info card for an exited run."
  (let ((id (or (alist-get 'id card) "")))
    (and (equal (alist-get 'kind card) "info")
         (or (string-prefix-p "exited:" id)
             (string-prefix-p "reload:exited:" id))
         t)))

(defun triage-card-has-step-p (card)
  "Return non-nil when CARD is an info card with a next step to plan."
  (and (equal (alist-get 'kind card) "info")
       (triage-card--nonempty-p (alist-get 'action (alist-get 'star card)))
       t))

(defun triage-card-other-allowed-p (card step)
  "Return non-nil when the Other answer is offered on CARD at STEP.
Port of otherAllowed in ui/app.js."
  (let* ((kind (alist-get 'kind card))
         (questions (alist-get 'questions card))
         (question (and questions (nth step questions))))
    (cond ((not (triage-card-answerable-p card)) nil)
          ((and (equal kind "finding") (null questions)) nil)
          ((and questions (null question)) nil)
          ((and question
                (assq 'allow_other question)
                (null (alist-get 'allow_other question)))
           nil)
          (t (and (alist-get 'allow_other card)
                  (member kind '("ask" "plan" "finding"))
                  t)))))

(defun triage-card-rec-index (options)
  "Return the index of the first recommended option in OPTIONS, or 0."
  (or (cl-position-if (lambda (option) (alist-get 'recommended option)) options)
      0))

(defun triage-card-option-task (option)
  "Return the task selected by OPTION's label.
Port of optionTask in ui/app.js."
  (let* ((label-text (if (stringp option)
                         option
                       (or (alist-get 'label option) "")))
         (label (downcase (string-trim-left label-text))))
    (cond ((not (string-prefix-p "investigate" label)) "act")
          ((string-search "fix" label) "fix")
          (t "investigate"))))

(defun triage-card-sorted-options (options)
  "Return OPTIONS as (INDEX . OPTION) pairs, recommended ones first.
INDEX is the option's position in OPTIONS.  Callers show INDEX+1 as its
key, or a middle dot when INDEX is 9 or more.  Otherwise order is kept."
  (let* ((indexed (cl-loop for option in options
                           for index from 0
                           collect (cons index option)))
         (recommended (lambda (pair) (alist-get 'recommended (cdr pair)))))
    (append (cl-remove-if-not recommended indexed)
            (cl-remove-if recommended indexed))))

(defun triage-card-less-p (card-a card-b)
  "Return non-nil when CARD-A precedes CARD-B.
Sort by priority, creation time and id."
  (let ((priority-a (or (alist-get 'priority card-a) 5))
        (priority-b (or (alist-get 'priority card-b) 5))
        (created-a (or (alist-get 'created_ms card-a) 0))
        (created-b (or (alist-get 'created_ms card-b) 0)))
    (cond ((/= priority-a priority-b) (< priority-a priority-b))
          ((/= created-a created-b) (< created-a created-b))
          (t (string< (or (alist-get 'id card-a) "")
                      (or (alist-get 'id card-b) ""))))))

(defun triage-card-sort (cards)
  "Return a sorted copy of CARDS, ordered by `triage-card-less-p'."
  (sort (copy-sequence cards) #'triage-card-less-p))

(defun triage-card-counts (cards)
  "Return how many of CARDS are of each kind, as an alist.
Port of countsFrom in ui/app.js."
  (mapcar (lambda (kind)
            (cons (intern kind)
                  (cl-count kind cards
                            :key (lambda (card) (alist-get 'kind card))
                            :test #'equal)))
          '("ask" "plan" "finding" "info")))

(defun triage-card-source-name (source)
  "Return the short name of SOURCE, an alist with uri, label, cwd and target.
Port of sourceName in ui/app.js."
  (let* ((label (or (alist-get 'label source) ""))
         (target (alist-get 'target source))
         (window (if target
                     (replace-regexp-in-string "\\.[^.]*\\'" "" target)
                   ""))
         (rest (if (and (not (string-empty-p window))
                        (string-prefix-p (concat window " ") label))
                   (substring label (1+ (length window)))
                 label))
         (title (car (split-string (triage-card--strip-pictographs rest)
                                   "[[:space:]]*·[[:space:]]*")))
         (cwd (alist-get 'cwd source))
         (base (and cwd (triage-card--cwd-base cwd)))
         (uri (alist-get 'uri source)))
    (cond ((triage-card--nonempty-p title) title)
          ((triage-card--nonempty-p base) base)
          ((triage-card--nonempty-p uri) uri)
          (t "unknown"))))

(defun triage-card-directory (cwd)
  "Return CWD with a leading home directory shortened to ~."
  (replace-regexp-in-string "\\`/\\(?:home\\|Users\\)/[^/]+\\(/\\|\\'\\)"
                            "~\\1" cwd t))

(defun triage-card-age (ms now-ms)
  "Return how long ago MS was, relative to NOW-MS, as a short string.
The result is \"now\", or minutes, hours or days with an m, h or d suffix.
Port of age in ui/app.js."
  (let ((seconds (max 0 (/ (- now-ms ms) 1000.0))))
    (cond ((< seconds 60) "now")
          ((< seconds 3600) (format "%dm" (floor seconds 60)))
          ((< seconds 86400) (format "%dh" (floor seconds 3600)))
          (t (format "%dd" (floor seconds 86400))))))

(defun triage-card-clock (ms now)
  "Return the time of MS as %H:%M on NOW's day, else as %a %H:%M.
MS is milliseconds since the epoch.  NOW is a float time."
  (let* ((time (/ ms 1000.0))
         (today (string= (format-time-string "%F" time)
                         (format-time-string "%F" now))))
    (format-time-string (if today "%H:%M" "%a %H:%M") time)))

(defun triage-card-later-choices (now)
  "Return the Later choices for NOW, a float time.
Each choice is a plist with :label, :minutes and :at, where :at is
a float time.  Port of laterChoices in ui/app.js."
  (let* ((decoded (decode-time now))
         (tomorrow-nine
          (let ((local (copy-sequence decoded)))
            (setf (nth 0 local) 0
                  (nth 1 local) 0
                  (nth 2 local) 9
                  (nth 3 local) (1+ (nth 3 local))
                  (nth 7 local) -1
                  (nth 8 local) nil)
            (float-time (encode-time local))))
         (minutes (max 1 (ceiling (/ (- tomorrow-nine now) 60.0)))))
    (list (list :label "1 hour" :minutes 60 :at (+ now 3600.0))
          (list :label "4 hours" :minutes 240 :at (+ now 14400.0))
          (list :label "Tomorrow" :minutes minutes :at tomorrow-nine))))

(defun triage-card--word-keys (word)
  "Return WORD's key candidates: downcase then upcase of each character."
  (mapcan (lambda (char) (list (downcase char) (upcase char)))
          word))

(defun triage-card-choice-keys (labels)
  "Return one single key per label in LABELS, mnemonic and unique.
A label takes the first free candidate its words offer in order, the
first word leading, the way Magit's dispatch menus key their commands.
Earlier labels keep their key, so every label gets its own.  Digits
finish the candidates when the letters run out."
  (let ((used ())
        (keys ()))
    (dolist (label labels (nreverse keys))
      (let ((candidates
             (append
              (mapcan #'triage-card--word-keys
                      (split-string label "[^[:alnum:]]+" t))
              (number-sequence ?1 ?9)))
            (key nil))
        (while (and candidates (null key))
          (unless (memq (car candidates) used)
            (setq key (car candidates)))
          (setq candidates (cdr candidates)))
        (unless key
          (error "No key left for choice %s" label))
        (push key used)
        (push key keys)))))

(defun triage-card-effort-levels (catalog selector)
  "Return the thinking levels that the model SELECTOR accepts in CATALOG.
A model that lists its efforts, even as an empty list, gives that list.
Otherwise the catalog's thinking levels apply.  Port of effortLevels in
ui/app.js."
  (let* ((models (alist-get 'models catalog))
         (model (or (triage-card--find-model models selector)
                    (triage-card--find-model models (alist-get 'default catalog)))))
    (if (assq 'efforts model)
        (alist-get 'efforts model)
      (alist-get 'thinking catalog))))

(defun triage-card-swipe-status (swipe fallback)
  "Return the outcome text of SWIPE, or FALLBACK when it has none.
A done swipe reads done, or done with its reason in parentheses.
Port of the status in historyItem in ui/app.js."
  (let ((action (alist-get 'action swipe))
        (reason (alist-get 'reason swipe)))
    (cond ((equal action "done")
           (if (triage-card--nonempty-p reason)
               (format "done (%s)" reason)
             "done"))
          ((triage-card--nonempty-p action) action)
          (t fallback))))

(defun triage-card-swipe-details (card swipe)
  "Return the details of SWIPE on CARD, joined with \" · \".
The details are the chosen option label, the typed text when it differs
from that label, and the note.  An empty string means none."
  (let* ((label (triage-card--option-label (alist-get 'options card)
                                           (alist-get 'option swipe)))
         (text (alist-get 'text swipe))
         (note (alist-get 'note swipe)))
    (string-join
     (seq-filter #'triage-card--nonempty-p
                 (list label
                       (and (not (equal text label)) text)
                       (and (triage-card--nonempty-p note)
                            (format "Note: %s" note))))
     " · ")))

(defun triage-card-result-message (card verb body choice)
  "Return the status line for VERB on CARD with BODY.
CHOICE is the Later plist when VERB is later.
Port resultMessage from ui/app.js."
  (let ((title (alist-get 'title card))
        (kind (alist-get 'kind card))
        (answers (alist-get 'answers body)))
    (cond
     ((equal verb "skip") (format "Skipped · %s" title))
     ((equal verb "later") (format "Later %s · %s" (plist-get choice :label) title))
     ((or (equal verb "dispatch")
          (and (equal verb "answer") answers (equal kind "finding")))
      (format "%s · %s"
              (triage-card--acting-verb (alist-get 'mode body) (alist-get 'task body))
              title))
     ((equal verb "close") (format "Closed · %s" title))
     ((equal verb "report") (format "Requested report · %s" title))
     ((equal verb "restore") (format "Restoring run · %s" title))
     ((equal kind "info") (format "Acknowledged · %s" title))
     (answers
      (format "Sent %s · %s"
              (string-join (seq-map-indexed
                            (lambda (answer index)
                              (triage-card--answer-label card answer index))
                            answers)
                           ", ")
              title))
     (t
      (format "%s %s · %s"
              (if (equal kind "finding") "Launched" "Sent")
              (or (triage-card--first-text
                   (alist-get 'text body)
                   (triage-card--option-label (alist-get 'options card)
                                              (alist-get 'option body)))
                  "Custom answer")
              title)))))

(defconst triage-card-actions
  (list
   (list 'plan :key "p"
         :label (lambda (card) (and (triage-card-has-step-p card) "Plan next step"))
         :command 'triage-inbox-plan)
   (list 'dispatch :key "d" :label "Dispatch" :command 'triage-inbox-dispatch)
   (list 'fix :key "f" :command 'triage-inbox-fix)
   (list 'act :key "A" :command 'triage-inbox-act)
   (list 'plan-into :key "P" :command 'triage-inbox-plan-into)
   (list 'do-next :key "D" :label "Do next step"
         :offered (lambda (card _step) (triage-card-has-step-p card))
         :command 'triage-inbox-do-next)
   (list 'close :key "C" :label "Acknowledge & close"
         :offered (lambda (card _step) (triage-card-pane card))
         :command 'triage-inbox-close)
   (list 'request :key "r" :label "Request final report"
         :offered (lambda (card _step)
                    (and (equal (alist-get 'kind card) "info")
                         (alist-get 'report_due card)
                         (triage-card-pane card)))
         :command 'triage-inbox-request-report)
   (list 'restore :key "R" :label "Restore run"
         :offered (lambda (card _step) (triage-card-restorable-p card))
         :command 'triage-inbox-restore)
   (list 'ack :key "a" :label "Acknowledge"
         :offered (lambda (card _step) (equal (alist-get 'kind card) "info"))
         :command 'triage-inbox-acknowledge)
   (list 'skip :key "s" :label "Skip" :command 'triage-inbox-skip)
   (list 'context :key "t" :label "Context" :command 'triage-inbox-context)
   (list 'later :key "l" :label "Later" :command 'triage-inbox-later)
   (list 'peek :key "v" :label "Peek"
         :offered (lambda (card _step) (triage-card-pane card))
         :command 'triage-inbox-peek)
   (list 'jump :key "J"
         :label (lambda (card) (if (triage-card-dsh-id card) "Open" "Jump"))
         :offered (lambda (card _step) (triage-card-pane card))
         :command 'triage-inbox-jump)
   (list 'note :key "n" :command 'triage-inbox-note)
   (list 'other :key "o" :offered #'triage-card-other-allowed-p
         :command 'triage-inbox-other)
   (list 'copy :key "w"
         :offered (lambda (card _step)
                    (and (equal (alist-get 'kind card) "info")
                         (triage-card--nonempty-p (alist-get 'body card))))
         :command 'triage-inbox-copy)
   (list 'executor :key "e"
         :offered (lambda (card _step)
                    (and (equal (alist-get 'kind card) "plan")
                         (triage-card-answerable-p card)))
         :command 'triage-inbox-executor)
   (list 'effort :key "E"
         :offered (lambda (card _step)
                    (and (equal (alist-get 'kind card) "plan")
                         (triage-card-answerable-p card)
                         (equal (alist-get 'engine card) "dsh")))
         :command 'triage-inbox-effort))
  "Every card action in dock order, one row per action.

Each row is (ID :key K :label L :offered P :command C).
L is a string, a function of the card, or absent.  An absent label means
the action has no dock button.  P is called as (P CARD STEP), and an
absent P means the action is always offered.")

(defun triage-card-offers-p (card id step)
  "Return non-nil when action ID is available on CARD at STEP."
  (let ((row (cdr (assq id triage-card-actions))))
    (and row
         (let ((offered (plist-get row :offered)))
           (or (null offered) (funcall offered card step)))
         t)))

(defun triage-card-label (card id step)
  "Return the button label of action ID on CARD at STEP, or nil.
The result is nil when the action is not offered or has no label."
  (when (triage-card-offers-p card id step)
    (let ((label (plist-get (cdr (assq id triage-card-actions)) :label)))
      (if (functionp label) (funcall label card) label))))

(defun triage-card-lead (card step)
  "Return the action that the dock of CARD leads with at STEP, or nil.
An info card leads with its first labeled action among plan, close and
ack.  An answer that waits in the pane leads with peek.  Otherwise there
is no lead."
  (cond ((equal (alist-get 'kind card) "info")
         (cl-find-if (lambda (id) (triage-card-label card id step))
                     '(plan close ack)))
        ((and (not (triage-card-answerable-p card))
              (triage-card-offers-p card 'peek step))
         'peek)))

(provide 'triage-card)
;;; triage-card.el ends here
