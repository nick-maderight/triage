;;; triage-inbox.el --- Live card deck for the triage inbox -*- lexical-binding: t; -*-

;;; Commentary:

;; The deck shows one queued card at a time in the `*triage*' buffer.
;; It ports the queue handling, pick, primary and action logic from
;; ui/app.js.  Card drawing lives in triage-render.el.  Every card action
;; goes through `triage-queue-act', which applies it optimistically.
;;
;; Keys come from `triage-card-actions', so the keymap, the dock and the
;; `?' menu cannot drift apart.

;;; Code:

(require 'cl-lib)
(require 'magit-section)
(require 'transient)
(require 'triage-api)
(require 'triage-card)
(require 'triage-queue)
(require 'triage-render)
(require 'triage-launch)
(require 'triage-peek)
(require 'triage-history)
(require 'triage-settings)

(defmacro triage-inbox--with-deck (&rest body)
  "Run BODY in the `*triage*' buffer when it is a live deck."
  (declare (indent 0) (debug t))
  `(when-let* ((buf (get-buffer "*triage*")))
     (with-current-buffer buf
       (when (derived-mode-p 'triage-inbox-mode)
         ,@body))))

(defvar-local triage-inbox--idx 0
  "Index of the shown card in `triage-queue-cards'.")

(defvar-local triage-inbox--shown-id nil
  "Id of the card the deck last drew.")

(defvar-local triage-inbox--shown-card nil
  "Card the deck last drew.")

(defvar-local triage-inbox--width nil
  "Text width the deck was last drawn for.")

(defvar-local triage-inbox--top nil
  "First card of the queue at the last update, or nil when it was empty.")

(defvar-local triage-inbox--guard-until 0.0
  "Float time before which digits and Enter are ignored.
The guard starts after a card is handled elsewhere, so a keypress
meant for the old card cannot answer the new one.")

(defvar-local triage-inbox--history
  (list :today nil :last-at nil :error nil :loaded nil :loading nil)
  "Summary of today's handled cards.
A plist with :today (the count), :last-at (milliseconds), :error,
:loaded and :loading.")

(defvar triage-inbox-mode-map
  (let ((map (define-keymap :parent magit-section-mode-map)))
    (dolist (entry triage-card-actions)
      (let ((spec (cdr entry)))
        (keymap-set map (plist-get spec :key) (plist-get spec :command))))
    (dolist (digit (number-sequence 1 9))
      (keymap-set map (number-to-string digit) #'triage-inbox-pick))
    (keymap-set map "RET" #'triage-inbox-primary)
    (dolist (key '("j" "<right>"))
      (keymap-set map key #'triage-inbox-next))
    (dolist (key '("k" "<left>"))
      (keymap-set map key #'triage-inbox-previous))
    (keymap-set map "g" #'triage-inbox-refresh)
    (keymap-set map "h" #'triage-history)
    (keymap-set map "c" #'triage-new-session)
    (keymap-set map "S" #'triage-settings)
    (keymap-set map "?" #'triage-inbox-menu)
    (keymap-set map "q" #'quit-window)
    map)
  "Keymap for `triage-inbox-mode'.
Every card key comes from `triage-card-actions', so the keys match the table.")

(define-derived-mode triage-inbox-mode magit-section-mode "Triage"
  "Major mode for the triage card deck in the `*triage*' buffer."
  (triage-render-setup-view)
  (add-hook 'window-size-change-functions #'triage-inbox--resized 90 t)
  (add-hook 'window-buffer-change-functions #'triage-inbox--resized 90 t)
  ;; The stretch lines the header up with the centered text below it.
  (setq-local header-line-format
              (list (propertize " " 'display '(space :align-to 0))
                    '(:eval (triage-inbox--header-line)))))

;;; State helpers

(defun triage-inbox--guarded-p ()
  "Return non-nil during the deck's key guard."
  (< (float-time) triage-inbox--guard-until))

(defun triage-inbox--step (card)
  "Return the current question step of CARD."
  (triage-draft-step (triage-queue-draft card)))

(defun triage-inbox--offers-p (id)
  "Return non-nil when action ID is available on the shown card.
Inspect the deck in its buffer, independent of the caller's buffer."
  (when-let* ((buf (get-buffer "*triage*")))
    (with-current-buffer buf
      (when-let* ((card triage-inbox--shown-card))
        (triage-card-offers-p card id (triage-inbox--step card))))))

(defun triage-inbox--card (&optional id)
  "Return the shown card and check offered action ID.
Signal `user-error' when no card is shown or ID is not offered."
  (let ((card triage-inbox--shown-card))
    (unless card
      (user-error "No card is shown"))
    (when (and id (not (triage-card-offers-p card id (triage-inbox--step card))))
      (user-error "%s is not offered on this card"
                  (or (triage-card-label card id (triage-inbox--step card))
                      (symbol-name id))))
    card))

(defun triage-inbox--command (id)
  "Return the command bound to the action ID in `triage-card-actions'."
  (or (plist-get (cdr (assq id triage-card-actions)) :command)
      (error "No command for triage action %s" id)))

(defun triage-inbox--or-null (string)
  "Return STRING, or :null when it is empty."
  (if (string-empty-p string) :null string))

(defun triage-inbox--note-value (draft)
  "Return the trimmed note of DRAFT, or :null when it is blank."
  (triage-inbox--or-null (string-trim (triage-draft-note draft))))

(defun triage-inbox--draft-blank-p (card)
  "Return non-nil when CARD has no note and no Other text."
  (let ((draft (triage-queue-draft card)))
    (and (string-empty-p (string-trim (triage-draft-note draft)))
         (string-empty-p (string-trim (triage-draft-other draft))))))

(defun triage-inbox--act (card verb body)
  "Send VERB with BODY for CARD through the optimistic queue action.
Signal a `user-error' when CARD is no longer in the queue."
  (or (triage-queue-act card verb body)
      (user-error "This card is no longer in the queue")))

(defun triage-inbox--read-only-notice (card)
  "Return the notice that explains how to answer CARD outside the deck."
  (cond ((triage-card-dsh-id card)
         "Answer in the dsh web UI. This prompt is read only here.")
        ((triage-card-ompsdk-id card)
         "Answer in the CLI. This prompt is read only here.")
        (t "Answer in the pane. This prompt is read only here.")))

(defun triage-inbox--set-history (&rest args)
  "Merge the key and value pairs in ARGS into the history summary."
  (let ((summary (copy-sequence triage-inbox--history)))
    (while args
      (setq summary (plist-put summary (pop args) (pop args))))
    (setq triage-inbox--history summary)))

;;; Queue update, port of applyQueue

(defun triage-inbox--on-update ()
  "Fold the latest queue into the open deck, as applyQueue does in app.js."
  (triage-inbox--with-deck
    (triage-inbox--apply)))

(defun triage-inbox--apply ()
  "Update the deck state from `triage-queue-cards' and redraw when needed."
  (let* ((cards triage-queue-cards)
         (old-id triage-inbox--shown-id)
         (old-card triage-inbox--shown-card)
         (old-idx triage-inbox--idx)
         (old-top triage-inbox--top)
         (pos (and old-id
                   (cl-position old-id cards
                                :key (lambda (card) (alist-get 'id card))
                                :test #'equal)))
         (idx old-idx)
         (remote-title nil))
    (cond
     (pos
      (setq idx pos))
     ((and old-id (gethash old-id triage-queue--inflight))
      ;; Our own action removed the card, so the next card takes its place.
      nil)
     (old-id
      (setq remote-title (triage-card-title-of old-card))))
    ;; A card that outranks the old lead takes the view, unless the shown card has text.
    (when (and old-top cards
               (triage-card-less-p (car cards) old-top)
               (or (null old-card) (triage-inbox--draft-blank-p old-card)))
      (setq idx 0))
    (setq idx (max 0 (min idx (1- (length cards)))))
    (setq triage-inbox--idx idx
          triage-inbox--top (car cards))
    (when (null cards)
      (when old-top
        (triage-inbox--set-history :loaded nil :error nil))
      (unless (or (plist-get triage-inbox--history :loaded)
                  (plist-get triage-inbox--history :loading))
        (triage-inbox--load-history)))
    (when remote-title
      (setq triage-inbox--guard-until (+ (float-time) 0.5))
      (message "Handled elsewhere · %s" remote-title))
    (let ((new-id (alist-get 'id (nth idx cards))))
      (if (and new-id (equal new-id old-id))
          ;; Card data never changes under an id, so the deck keeps its drawing.
          (progn
            (setq triage-inbox--shown-card (nth idx cards))
            (force-mode-line-update))
        (triage-inbox--render)))))

;;; History summary

(defun triage-inbox--last-at (body)
  "Return the newest swipe time in the history BODY, in milliseconds."
  (cl-loop for row in (alist-get 'cards body)
           for at = (alist-get 'at_ms (alist-get 'swipe row))
           when at maximize at))

(defun triage-inbox--load-history ()
  "Fetch today's handled count for the empty deck."
  (triage-inbox--set-history :loading t)
  (triage-history-fetch
   (lambda (body)
     (triage-inbox--with-deck
       (triage-inbox--set-history
        :today (alist-get 'today_count body)
        :last-at (triage-inbox--last-at body)
        :error nil :loaded t :loading nil)
       (triage-inbox--render)))
   (lambda (_status text)
     (triage-inbox--with-deck
       (triage-inbox--set-history :error text :loaded t :loading nil)
       (triage-inbox--render)))))

;;; Drawing

(defun triage-inbox--offline-p ()
  "Return non-nil when the deck is empty and the daemon is unreachable."
  (and (null triage-queue-cards)
       (eq triage-queue-connection 'reconnecting)
       (or (not triage-queue-initialized)
           (and triage-queue-unreachable-since
                (> (- (float-time) triage-queue-unreachable-since) 4)))))

(defun triage-inbox--action-fn (card)
  "Return a callback for CARD's buttons in `triage-render-card'.
Map action ids to command thunks.  Option picks arrive as `(pick . INDEX)'
and review rows as `(step . INDEX)'."
  (lambda (id)
    (pcase id
      (`(pick . ,i) (lambda () (triage-inbox--pick card i)))
      (`(step . ,k) (lambda () (triage-inbox--go-step card k)))
      ('send-answers (lambda () (triage-inbox--send-answers card)))
      (_ (let ((command (triage-inbox--command id)))
           (lambda () (funcall command)))))))

(defun triage-inbox--hints (pairs)
  "Return PAIRS, each (KEY . TEXT), as one line of key hints."
  (mapconcat (lambda (pair)
               (concat (propertize (car pair) 'font-lock-face 'triage-key)
                       " " (cdr pair)))
             pairs "   "))

(defun triage-inbox--insert-history ()
  "Insert the history lines of the empty deck."
  (let ((failure (plist-get triage-inbox--history :error)))
    (cond
     (failure
      (insert (propertize (format "History unavailable: %s" failure)
                          'font-lock-face 'triage-status-negative)
              "\n"))
     ((not (plist-get triage-inbox--history :loaded))
      (insert (propertize "Loading recent history…" 'font-lock-face 'triage-muted)
              "\n"))
     (t
      (insert (format "Handled today: %d\n"
                      (or (plist-get triage-inbox--history :today) 0)))
      (insert (format "Last handled: %s\n"
                      (if-let* ((at (plist-get triage-inbox--history :last-at)))
                          (triage-card-clock at (float-time))
                        "No recent history.")))))))

(defun triage-inbox--insert-empty ()
  "Insert the empty deck: inbox zero, recent history and key hints."
  (insert (propertize "Inbox zero" 'font-lock-face 'triage-title) "\n")
  (insert (propertize "Nothing is waiting on you." 'font-lock-face 'triage-muted)
          "\n\n")
  (triage-inbox--insert-history)
  (insert "\n")
  (dolist (row '(("ask" . "Choose an answer")
                 ("plan" . "Review a plan")
                 ("finding" . "Review suggested work")
                 ("info" . "Read a result")))
    (insert (propertize (format "%-9s" (car row)) 'font-lock-face 'triage-muted)
            (cdr row) "\n"))
  (insert "\n"
          (triage-inbox--hints '(("c" . "new session") ("h" . "history")
                                 ("g" . "refresh") ("?" . "keys")))
          "\n"))

(defun triage-inbox--insert-offline ()
  "Insert the view shown while the daemon is unreachable."
  (insert (propertize "Daemon unreachable" 'font-lock-face 'triage-status-negative)
          "\n")
  (insert (propertize "Retrying automatically." 'font-lock-face 'triage-muted)
          "\n\n")
  (triage-render-button "Retry now" "g" nil #'triage-inbox-refresh)
  (insert "\n"))

(defun triage-inbox--render ()
  "Redraw the deck for the card at `triage-inbox--idx'.
Point and window starts survive a redraw of the same card.  Any other
redraw starts at the top."
  (let* ((cards triage-queue-cards)
         (idx (max 0 (min triage-inbox--idx (1- (length cards)))))
         (card (nth idx cards))
         (id (alist-get 'id card))
         (same (and id (equal id triage-inbox--shown-id)))
         (pos (point))
         (windows (get-buffer-window-list (current-buffer) nil t))
         (starts (mapcar (lambda (window) (cons window (window-start window)))
                         windows)))
    (setq triage-inbox--idx idx
          triage-inbox--shown-id id
          triage-inbox--shown-card card
          triage-inbox--width (triage-render--width))
    (let ((inhibit-read-only t))
      (erase-buffer)
      (setq magit-root-section nil)
      (magit-insert-section (triage-inbox-root)
        (insert "\n")
        (cond
         (card (triage-render-card card (triage-queue-draft card)
                                   (triage-inbox--action-fn card)))
         ((triage-inbox--offline-p) (triage-inbox--insert-offline))
         (t (triage-inbox--insert-empty)))))
    (if same
        (progn
          (goto-char (min pos (point-max)))
          (dolist (entry starts)
            (set-window-start (car entry) (min (cdr entry) (point-max)) t)))
      (goto-char (point-min))
      (dolist (window windows)
        (set-window-start window (point-min) t)))
    (force-mode-line-update)))

(defun triage-inbox--resized (window)
  "Redraw the deck in WINDOW when its text width changed since the last draw.
The facts line and the dock break their lines for that width."
  (with-current-buffer (window-buffer window)
    (unless (eql triage-inbox--width (triage-render--width))
      (triage-inbox--render))))

(defun triage-inbox--rail (cards idx)
  "Return one bar per card in CARDS, the bar at IDX drawn as current.
A card's bar height comes from its priority, 1 tallest and 5 shortest."
  (apply #'concat
         (cl-loop for card in cards
                  for i from 0
                  collect (let ((level (max 1 (min 5 (or (alist-get 'priority card) 5)))))
                            (propertize (substring "█▆▄▂▁" (1- level) level)
                                        'font-lock-face
                                        (if (= i idx) 'triage-rail-current 'triage-rail))))))

(defun triage-inbox--header-line ()
  "Return the deck header: position, rail, age, counts and connection.
A header wider than the window drops the counts first.  The key hint
goes next and the age goes last."
  (let* ((cards triage-queue-cards)
         (count (length cards))
         (idx (max 0 (min triage-inbox--idx (1- count))))
         (counts (format "ask %d · plan %d · finding %d · info %d"
                         (alist-get 'ask triage-queue-counts 0)
                         (alist-get 'plan triage-queue-counts 0)
                         (alist-get 'finding triage-queue-counts 0)
                         (alist-get 'info triage-queue-counts 0)))
         (connection (pcase triage-queue-connection
                       ('connected "Connected")
                       ('reconnecting "Reconnecting")
                       (_ "Connecting")))
         (waiting (and cards
                       (format "waiting %s"
                               (triage-card-age
                                (or (alist-get 'created_ms (nth idx cards)) 0)
                                (round (* 1000 (float-time)))))))
         (parts (if (null cards)
                    (list counts connection)
                  (list (format "%d of %d" (1+ idx) count)
                        (triage-inbox--rail cards idx)
                        waiting counts connection "? keys")))
         ;; The header starts at the text column and may run into the right margin.
         (room (+ (window-body-width) (or (cdr (window-margins)) 0))))
    (dolist (part (list counts "? keys" waiting))
      (when (> (string-width (string-join parts "   ")) room)
        (setq parts (delete part parts))))
    (string-join parts "   ")))

;;; Picking, steps and primary, ports of pick, pickStep and primary

(defun triage-inbox--go-step (card step)
  "Move the draft of CARD to STEP and redraw, as goStep does in app.js."
  (let* ((draft (triage-queue-draft card))
         (answers (triage-draft-answers draft))
         (answer (and answers (< step (length answers)) (aref answers step))))
    (setf (triage-draft-step draft) step
          (triage-draft-other draft) (or (alist-get 'text answer) ""))
    (triage-inbox--render)))

(defun triage-inbox--answer-step (card answer)
  "Store ANSWER for the current question of CARD and advance.
ANSWER is `((option . I))' or `((text . S))'.  The next unanswered
question comes next, else the review step, as answerStep does."
  (let* ((draft (triage-queue-draft card))
         (count (length (alist-get 'questions card)))
         (step (triage-draft-step draft))
         (answers (or (triage-draft-answers draft)
                      (setf (triage-draft-answers draft) (make-vector count nil)))))
    (aset answers step answer)
    (triage-inbox--go-step
     card
     (or (cl-loop for i from (1+ step) below count
                  when (null (aref answers i)) return i)
         count))))

(defun triage-inbox--other (card)
  "Ask for Other text on CARD and answer with it.
Signal a `user-error' when the text is blank."
  (let* ((draft (triage-queue-draft card))
         (typed (read-string "Your answer: " (triage-draft-other draft)))
         (text (string-trim typed)))
    (when (string-empty-p text)
      (user-error "Type an answer first"))
    (setf (triage-draft-other draft) typed)
    (if (alist-get 'questions card)
        (triage-inbox--answer-step card `((text . ,text)))
      (triage-inbox--act card "answer"
                         `((option . :null) (text . ,text)
                           (note . ,(triage-inbox--note-value draft)))))))

(defun triage-inbox--pick-step (card i)
  "Handle digit index I on a question step of CARD.
Signal a `user-error' when I names no option and not Other."
  (let* ((draft (triage-queue-draft card))
         (step (triage-draft-step draft))
         (questions (alist-get 'questions card))
         (question (nth step questions)))
    (if (null question)
        (if (< i (length questions))
            (triage-inbox--go-step card i)
          (user-error "No option %d" (1+ i)))
      (let ((options (alist-get 'options question)))
        (cond
         ((< i (length options))
          (triage-inbox--answer-step card (list (cons 'option i))))
         ((and (= i (length options)) (triage-card-other-allowed-p card step))
          (triage-inbox--other card))
         (t (user-error "No option %d" (1+ i))))))))

(defun triage-inbox--refinement (draft)
  "Return the trimmed Other text of DRAFT, asking for it when blank.
Signal a `user-error' when the prompt is left empty too."
  (let ((text (string-trim (triage-draft-other draft))))
    (when (string-empty-p text)
      (setf (triage-draft-other draft)
            (read-string "Refinement: " (triage-draft-other draft)))
      (setq text (string-trim (triage-draft-other draft)))
      (when (string-empty-p text)
        (user-error "Add refinement text in Other before choosing Refine")))
    text))

(defun triage-inbox--answer-option (card option i)
  "Answer CARD with OPTION, the option at index I, as pick does in app.js.
A finding opens the dispatch sheet.  A plan Refine needs Other text, and
a plan Approve adds the execution model."
  (let ((kind (alist-get 'kind card))
        (label (alist-get 'label option))
        (draft (triage-queue-draft card)))
    (if (equal kind "finding")
        (triage-launch-dispatch card nil (triage-card-option-task option) :option i)
      (let* ((refine (and (equal kind "plan") (equal label "Refine")))
             (text (and refine (triage-inbox--refinement draft)))
             (body `((option . ,i)
                     (text . ,(or text :null))
                     (note . ,(triage-inbox--note-value draft)))))
        (when (and (equal kind "plan") (equal label "Approve"))
          (setq body
                (append body
                        `((executor . ,(triage-inbox--or-null (triage-draft-executor draft)))
                          (executor_thinking
                           . ,(triage-inbox--or-null (triage-draft-executor-thinking draft)))))))
        (triage-inbox--act card "answer" body)))))

(defun triage-inbox--pick (card i)
  "Handle the digit index I on CARD, as pick does in app.js.
Do nothing while the guard runs.  Otherwise explain any digit that
does nothing."
  (unless (triage-inbox--guarded-p)
    (let ((step (triage-inbox--step card))
          (options (alist-get 'options card)))
      (cond
       ((and (alist-get 'questions card) (triage-card-answerable-p card))
        (triage-inbox--pick-step card i))
       ((and (= i (length options)) (triage-card-other-allowed-p card step))
        (triage-inbox--other card))
       ((not (triage-card-answerable-p card))
        (message "%s" (triage-inbox--read-only-notice card)))
       ((equal (alist-get 'kind card) "info") nil)
       ((null (nth i options)) (user-error "No option %d" (1+ i)))
       (t (triage-inbox--answer-option card (nth i options) i))))))

(defun triage-inbox--send-answers (card)
  "Send every stored answer for CARD, or say which are missing.
A finding opens the dispatch sheet with the answers instead."
  (let* ((draft (triage-queue-draft card))
         (answers (triage-draft-answers draft)))
    (when (or (null answers) (cl-some #'null answers))
      (user-error "Answer every question first"))
    (if (equal (alist-get 'kind card) "finding")
        (triage-launch-dispatch card nil "act" :answers answers)
      (triage-inbox--act card "answer"
                         `((answers . ,answers)
                           (note . ,(triage-inbox--note-value draft)))))))

(defun triage-inbox--primary-step (card)
  "Run the primary action of CARD's current question step, as app.js does."
  (let ((questions (alist-get 'questions card))
        (step (triage-inbox--step card)))
    (if (>= step (length questions))
        (triage-inbox--send-answers card)
      (triage-inbox--pick card
                          (triage-card-rec-index
                           (alist-get 'options (nth step questions)))))))

(defun triage-inbox--primary (card)
  "Run CARD's lead, else the step action or recommended option."
  (let ((lead (triage-card-lead card (triage-inbox--step card))))
    (cond
     (lead (funcall (triage-inbox--command lead)))
     ((and (alist-get 'questions card) (triage-card-answerable-p card))
      (triage-inbox--primary-step card))
     (t (triage-inbox--pick card (triage-card-rec-index (alist-get 'options card)))))))

;;; Commands

(defun triage-inbox-next ()
  "Show the next card in the deck."
  (interactive)
  (triage-inbox--move 1))

(defun triage-inbox-previous ()
  "Show the previous card in the deck."
  (interactive)
  (triage-inbox--move -1))

(defun triage-inbox--move (delta)
  "Move the shown card by DELTA positions, clamped to the deck, and redraw."
  (setq triage-inbox--idx
        (max 0 (min (+ triage-inbox--idx delta) (1- (length triage-queue-cards)))))
  (triage-inbox--render))

(defun triage-inbox-pick ()
  "Pick the option whose digit key was just pressed."
  (interactive)
  (triage-inbox--pick (triage-inbox--card) (- last-command-event ?1)))

(defun triage-inbox-primary ()
  "Run the shown card's primary action, which Enter triggers."
  (interactive)
  (unless (triage-inbox--guarded-p)
    (triage-inbox--primary (triage-inbox--card))))

(defun triage-inbox-plan ()
  "Open the plan sheet, with the Act task, for the shown card."
  (interactive)
  (let ((card (triage-inbox--card 'plan)))
    (triage-launch-dispatch card "plan" "act" :new-window (triage-card-has-step-p card))))

(defun triage-inbox-dispatch ()
  "Open the Dispatch sheet with Investigate on the engine's default mode."
  (interactive)
  (triage-launch-dispatch (triage-inbox--card 'dispatch) nil "investigate"))

(defun triage-inbox-fix ()
  "Open the Dispatch sheet with Fix on the engine's default mode."
  (interactive)
  (triage-launch-dispatch (triage-inbox--card 'fix) nil "fix"))

(defun triage-inbox-act ()
  "Open the Dispatch sheet with Act on the engine's default mode."
  (interactive)
  (triage-launch-dispatch (triage-inbox--card 'act) nil "act"))

(defun triage-inbox-plan-into ()
  "Open the Plan into sheet with Act for the shown card."
  (interactive)
  (triage-launch-dispatch (triage-inbox--card 'plan-into) "plan-into" "act"))

(defun triage-inbox-do-next ()
  "Open the Goal sheet with Act to carry out the next step.
The session opens in its own window."
  (interactive)
  (triage-launch-dispatch (triage-inbox--card 'do-next) "goal" "act" :new-window t))

(defun triage-inbox-close ()
  "Acknowledge the shown card and close its pane session."
  (interactive)
  (let ((card (triage-inbox--card 'close)))
    (triage-inbox--act card "close"
                       `((note . ,(triage-inbox--note-value (triage-queue-draft card)))))))

(defun triage-inbox-acknowledge ()
  "Acknowledge the shown info card."
  (interactive)
  (let ((card (triage-inbox--card 'ack)))
    (triage-inbox--act card "answer"
                       `((option . :null) (text . :null)
                         (note . ,(triage-inbox--note-value (triage-queue-draft card)))))))

(defun triage-inbox-request-report ()
  "Ask the shown card's pane session for its final report."
  (interactive)
  (triage-inbox--act (triage-inbox--card 'request) "report" nil))

(defun triage-inbox-restore ()
  "Restore the run behind the shown card."
  (interactive)
  (triage-inbox--act (triage-inbox--card 'restore) "restore" nil))

(defun triage-inbox-open-cli ()
  "Open the ompsdk session behind the shown card in an omp window."
  (interactive)
  (triage-inbox--act (triage-inbox--card 'cli) "cli" nil))

(defun triage-inbox-skip ()
  "Skip the shown card."
  (interactive)
  (triage-inbox--act (triage-inbox--card 'skip) "skip" nil))

(defun triage-inbox-later ()
  "Defer the shown card for one hour."
  (interactive)
  (triage-inbox--act (triage-inbox--card 'later) "later" '((minutes . 60))))

(defun triage-inbox-note ()
  "Edit the draft note of the shown card."
  (interactive)
  (let* ((card (triage-inbox--card 'note))
         (draft (triage-queue-draft card)))
    (setf (triage-draft-note draft) (read-string "Note: " (triage-draft-note draft)))
    (triage-inbox--render)))

(defun triage-inbox-other ()
  "Answer the shown card with text typed into Other."
  (interactive)
  (triage-inbox--other (triage-inbox--card 'other)))

(defun triage-inbox-copy ()
  "Copy the body of the shown info card to the kill ring."
  (interactive)
  (kill-new (alist-get 'body (triage-inbox--card 'copy)))
  (message "Reply copied."))

(defun triage-inbox-executor ()
  "Choose the execution model for the shown plan card.
The choices come from the engine's catalog."
  (interactive)
  (let* ((card (triage-inbox--card 'executor))
         (id (alist-get 'id card))
         (draft (triage-queue-draft card))
         (engine (or (alist-get 'engine card) "omp"))
         (dsh (equal engine "dsh")))
    (triage-launch--with-catalog
     engine
     (lambda (catalog)
       (when (equal id (alist-get 'id triage-inbox--shown-card))
         (let* ((models (alist-get 'execution_models catalog))
                (default (format "Default (%s)"
                                 (if dsh "current model" "current Plan Review role")))
                (names (mapcar (lambda (model)
                                 (cons (alist-get 'selector model) (alist-get 'name model)))
                               models))
                (completion-extra-properties
                 (list :annotation-function
                       (lambda (selector)
                         (when-let* ((name (cdr (assoc selector names))))
                           (concat "  " name)))))
                (choice (completing-read "Execution model: " (cons default (mapcar #'car names))
                                         nil t nil nil default))
                (selector (if (equal choice default) "" choice)))
           (setf (triage-draft-executor draft) selector)
           (when (and dsh
                      (not (member (triage-draft-executor-thinking draft)
                                   (triage-card-effort-levels catalog selector))))
             (setf (triage-draft-executor-thinking draft) ""))
           (triage-inbox--render)))))))

(defun triage-inbox-effort ()
  "Choose the execution effort for the shown plan card.
Signal a `user-error' until an execution model is chosen."
  (interactive)
  (let* ((card (triage-inbox--card 'effort))
         (id (alist-get 'id card))
         (draft (triage-queue-draft card))
         (executor (triage-draft-executor draft)))
    (when (string-empty-p executor)
      (user-error "Choose an execution model first (e)"))
    (triage-launch--with-catalog
     (or (alist-get 'engine card) "omp")
     (lambda (catalog)
       (when (equal id (alist-get 'id triage-inbox--shown-card))
         (let* ((default "Default (Planner effort)")
                (levels (triage-card-effort-levels catalog executor))
                (choice (triage-launch--read-level
                         "Execution effort: " default levels)))
           (setf (triage-draft-executor-thinking draft) (or choice ""))
           (triage-inbox--render)))))))

(defun triage-inbox-peek ()
  "Open a live peek of the shown card's pane."
  (interactive)
  (triage-peek-open (triage-inbox--card 'peek)))

(defun triage-inbox-context ()
  "Open the thread of cards that led to the shown card."
  (interactive)
  (triage-context-open (alist-get 'id (triage-inbox--card 'context))))

(defun triage-inbox-jump ()
  "Jump to the shown card's pane, or open its dsh session on the desktop."
  (interactive)
  (let ((card (triage-inbox--card 'jump)))
    (if (triage-card-dsh-id card)
        (triage-api-request
         'post "/v1/dsh/open"
         :then (lambda (_body) (message "Opened dsh on the desktop."))
         :else (lambda (_status text)
                 (message "%s" (propertize (format "Open failed: %s" text) 'face 'error))))
      (let ((pane (triage-card-pane card))
            (target (alist-get 'target (alist-get 'source card))))
        (triage-api-request
         'post (triage-api-session-path pane "jump")
         :then (lambda (_body) (message "Jumped to %s" (or target pane)))
         :else (lambda (_status text)
                 (message "%s" (propertize text 'face 'error))))))))

(defun triage-inbox-refresh ()
  "Retry the queue stream now and refresh the deck."
  (interactive)
  (setq triage-queue--retry-delay 1)
  (triage-queue--reconnect))

;;; Menu

(transient-define-prefix triage-inbox-menu ()
  "Every key a triage card answers to, grouped by what it does."
  [["Answer"
    (:info "1-9 pick an option · RET primary")
    ("o" "Other" triage-inbox-other
     :inapt-if-not (lambda () (triage-inbox--offers-p 'other)))
    ("a" "Acknowledge" triage-inbox-acknowledge
     :inapt-if-not (lambda () (triage-inbox--offers-p 'ack)))
    ("C" "Acknowledge & close" triage-inbox-close
     :inapt-if-not (lambda () (triage-inbox--offers-p 'close)))
    ("s" "Skip" triage-inbox-skip
     :inapt-if-not (lambda () (triage-inbox--offers-p 'skip)))
    ("l" "Later" triage-inbox-later
     :inapt-if-not (lambda () (triage-inbox--offers-p 'later)))
    ("n" "Note" triage-inbox-note
     :inapt-if-not (lambda () (triage-inbox--offers-p 'note)))
    ("w" "Copy reply" triage-inbox-copy
     :inapt-if-not (lambda () (triage-inbox--offers-p 'copy)))
    ("e" "Execution model" triage-inbox-executor
     :inapt-if-not (lambda () (triage-inbox--offers-p 'executor)))
    ("E" "Execution effort" triage-inbox-effort
     :inapt-if-not (lambda () (triage-inbox--offers-p 'effort)))]
   ["Launch"
    ("d" "Dispatch" triage-inbox-dispatch
     :inapt-if-not (lambda () (triage-inbox--offers-p 'dispatch)))
    ("f" "Fix" triage-inbox-fix
     :inapt-if-not (lambda () (triage-inbox--offers-p 'fix)))
    ("A" "Act" triage-inbox-act
     :inapt-if-not (lambda () (triage-inbox--offers-p 'act)))
    ("P" "Plan into" triage-inbox-plan-into
     :inapt-if-not (lambda () (triage-inbox--offers-p 'plan-into)))
    ("p" "Plan" triage-inbox-plan
     :inapt-if-not (lambda () (triage-inbox--offers-p 'plan)))
    ("D" "Do next step" triage-inbox-do-next
     :inapt-if-not (lambda () (triage-inbox--offers-p 'do-next)))]
   ["Session"
    ("v" "Peek" triage-inbox-peek
     :inapt-if-not (lambda () (triage-inbox--offers-p 'peek)))
    ("t" "Context" triage-inbox-context
     :inapt-if-not (lambda () (triage-inbox--offers-p 'context)))
    ("J" "Jump" triage-inbox-jump
     :inapt-if-not (lambda () (triage-inbox--offers-p 'jump)))
    ("r" "Request final report" triage-inbox-request-report
     :inapt-if-not (lambda () (triage-inbox--offers-p 'request)))
    ("R" "Restore run" triage-inbox-restore
     :inapt-if-not (lambda () (triage-inbox--offers-p 'restore)))
    ("O" "Open in CLI" triage-inbox-open-cli
     :inapt-if-not (lambda () (triage-inbox--offers-p 'cli)))]
   ["Inbox"
    ("j" "Next card" triage-inbox-next)
    ("k" "Previous card" triage-inbox-previous)
    ("g" "Refresh" triage-inbox-refresh)
    ("h" "History" triage-history)
    ("c" "New session" triage-new-session)
    ("S" "Settings" triage-settings)
    ("q" "Quit" quit-window)]])

(provide 'triage-inbox)
;;; triage-inbox.el ends here
