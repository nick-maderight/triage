;;; triage-render.el --- Render triage cards, context items and Markdown -*- lexical-binding: t; -*-

;;; Commentary:

;; View code for the inbox and the context sheet.  Everything here inserts text
;; into the current buffer.  Card actions reach the inbox through the ACTIONS-FN
;; callback, so this file never requires the inbox.

;;; Code:

(require 'cl-lib)
(require 'goto-addr)
(require 'json)
(require 'magit-section)
(require 'subr-x)
(require 'triage-card)
(require 'triage-queue)

(defvar markdown-hide-urls)
(defvar markdown-hide-markup)
(defvar markdown-fontify-code-blocks-natively)
(declare-function gfm-mode "markdown-mode")

(defface triage-status-positive '((t :inherit success))
  "Face for a positive card status label."
  :group 'triage)

(defface triage-status-warning '((t :inherit warning))
  "Face for a warning card status label."
  :group 'triage)

(defface triage-status-negative '((t :inherit error))
  "Face for a negative card status label."
  :group 'triage)

(defface triage-status-neutral '((t :inherit shadow))
  "Face for a neutral card status label."
  :group 'triage)

(defface triage-title '((t :weight bold :height 1.15))
  "Face for the title line of a card."
  :group 'triage)

(defface triage-muted '((t :inherit shadow))
  "Face for secondary text."
  :group 'triage)

(defface triage-key '((t :inherit help-key-binding))
  "Face for the key that triggers a button."
  :group 'triage)

(defface triage-primary '((t :inherit (bold link)))
  "Face for the primary button of a card."
  :group 'triage)

(defface triage-recommended '((t :inherit success))
  "Face for the Recommended tag on an option."
  :group 'triage)

(defface triage-rail '((t :inherit shadow))
  "Face for the priority rail characters of other cards."
  :group 'triage)

(defface triage-rail-current '((t :inherit (bold link)))
  "Face for the priority rail character of the shown card."
  :group 'triage)

(defun triage-render--face (text face)
  "Return TEXT with FACE applied through `font-lock-face'.
Return TEXT unchanged when FACE is nil."
  (if face (propertize text 'font-lock-face face) text))

(defun triage-render--present-p (string)
  "Return non-nil when STRING is a string with visible text."
  (and (stringp string) (not (string-blank-p string))))

(defun triage-render--digit (index)
  "Return the key for the option at INDEX: its digit, or \"·\" from ninth on."
  (if (< index 9) (number-to-string (1+ index)) "·"))

(defconst triage-render--indent "  "
  "Indent of the text under a section heading.")

(defun triage-render--gap ()
  "End the block before point with one blank line, unless it has one."
  (unless (or (< (- (point) (point-min)) 2)
              (equal (buffer-substring-no-properties (- (point) 2) (point)) "\n\n"))
    (insert "\n")))

(defun triage-render--width ()
  "Return the text width of the current buffer's window, at most `fill-column'."
  (let ((window (get-buffer-window nil t)))
    (if window (min fill-column (window-body-width window)) fill-column)))

(defun triage-render--fit-window (window)
  "Center the text of WINDOW in a column at most `fill-column' wide."
  (let* ((width (buffer-local-value 'fill-column (window-buffer window)))
         (margin (max 2 (/ (- (window-total-width window) width) 2))))
    (set-window-margins window margin margin)))

(defun triage-render-setup-view ()
  "Make the current buffer a centered reading view that wraps at words.
Wrapped lines keep their indent."
  (visual-line-mode 1)
  (visual-wrap-prefix-mode 1)
  (add-hook 'window-size-change-functions #'triage-render--fit-window nil t)
  (add-hook 'window-buffer-change-functions #'triage-render--fit-window nil t))

(defun triage-render--tone-face (tone)
  "Return the status face for TONE, neutral when TONE is nil."
  (pcase tone
    ('positive 'triage-status-positive)
    ('warning 'triage-status-warning)
    ('negative 'triage-status-negative)
    (_ 'triage-status-neutral)))

(defun triage-render--json-p (text)
  "Return non-nil when TEXT is valid JSON."
  (condition-case nil
      (let ((parsed (json-parse-string text))) (or parsed t))
    (error nil)))

(defun triage-render--gfm (text)
  "Return TEXT fontified by `gfm-mode', each face copied to `font-lock-face'."
  (with-temp-buffer
    (insert text)
    (let ((markdown-hide-urls nil)
          (markdown-hide-markup nil)
          (markdown-fontify-code-blocks-natively nil))
      (delay-mode-hooks (gfm-mode))
      (font-lock-ensure))
    (let ((result (buffer-substring-no-properties (point-min) (point-max)))
          (pos (point-min)))
      (while (< pos (point-max))
        (let ((next (next-single-property-change pos 'face nil (point-max)))
              (face (get-text-property pos 'face)))
          (when face
            (put-text-property (- pos (point-min)) (- next (point-min))
                               'font-lock-face face result))
          (setq pos next)))
      result)))

(defun triage-render-markdown (text)
  "Return TEXT as a propertized string ready for display.
JSON becomes one pretty-printed block.  Other text is fontified by `gfm-mode'
when markdown-mode is installed.  Otherwise TEXT is returned unchanged."
  (let ((trimmed (string-trim text)))
    (cond
     ((and (string-match-p "\\`[[{]" trimmed) (triage-render--json-p trimmed))
      (with-temp-buffer
        (insert trimmed)
        (json-pretty-print-buffer)
        (buffer-string)))
     ((require 'markdown-mode nil t) (triage-render--gfm text))
     (t text))))

(defun triage-render--insert-markdown (text &optional indent)
  "Insert TEXT as Markdown indented by INDENT, and make its links clickable.
INDENT defaults to `triage-render--indent'.  The text ends the line."
  (let ((start (point))
        (end (progn (insert (string-trim-right (triage-render-markdown text)) "\n")
                    (point-marker))))
    (save-excursion
      (goto-char start)
      (while (< (point) end)
        (unless (eolp) (insert (or indent triage-render--indent)))
        (forward-line 1)))
    (goto-address-fontify start end)
    (set-marker end nil)))

(defun triage-render-button (label key face action)
  "Insert LABEL as a button preceded by KEY.
The button calls ACTION.  KEY uses `triage-key'; FACE styles the button.
ACTION takes no arguments."
  (insert (triage-render--face key 'triage-key) " ")
  (apply #'insert-text-button label
         'action (lambda (_button) (funcall action))
         'follow-link t
         (and face (list 'face face))))

(defun triage-render--question-text (question)
  "Return the text that names QUESTION, as the web inbox's stepTitle does."
  (if (alist-get 'star question)
      (alist-get 'label question)
    (alist-get 'question question)))

(defun triage-render--title (card asking step question status)
  "Return the title line for CARD.
ASKING is non-nil when the card walks its questions, STEP is the draft step,
QUESTION is the current question and STATUS is the `triage-card-status' plist."
  (let ((count (length (alist-get 'questions card))))
    (cond
     ((not asking) (plist-get status :title))
     ((>= step count) "Review answers")
     (t (triage-render--question-text question)))))

(defun triage-render--facts (card)
  "Insert the muted facts line for CARD."
  (let* ((source (alist-get 'source card))
         (cwd (alist-get 'cwd source))
         (target (alist-get 'target source))
         (created (alist-get 'created_ms card))
         (kind (concat (alist-get 'kind card)
                       (if (equal (alist-get 'level card) "error") " error" "")))
         (facts
          (delq nil
                (list (cons "From" (triage-card-source-name source))
                      (and (triage-render--present-p cwd)
                           (cons "Directory" (triage-card-directory cwd)))
                      (and target (cons "Pane" target))
                      (cons "Kind" kind)
                      (cons nil (format "P%s" (alist-get 'priority card)))
                      (and created
                           (cons "Created" (triage-card-clock created (float-time))))))))
    (dolist (fact facts)
      (let ((text (if (car fact)
                      (concat (triage-render--face (car fact) 'triage-muted)
                              " " (cdr fact))
                    (cdr fact))))
        (unless (bolp)
          (triage-render--separate (string-width text)
                                   (triage-render--face " · " 'triage-muted)))
        (insert text)))
    (insert "\n")
    (triage-render--gap)))

(defun triage-render--star (key heading text)
  "Insert a section named KEY with HEADING whose Markdown body is TEXT."
  (magit-insert-section (triage-star key)
    (magit-insert-heading heading)
    (triage-render--insert-markdown text)
    (triage-render--gap)))

(defun triage-render--stars (card star)
  "Insert the situation, plan, task and result sections of CARD.
STAR is the star in effect: the current question's, else the card's."
  (let* ((kind (alist-get 'kind card))
         (situation (alist-get 'situation star))
         (task (alist-get 'task star))
         (result (alist-get 'result star))
         (body (alist-get 'body card))
         (info-body (and (equal kind "info") (triage-render--present-p body))))
    (when (triage-render--present-p situation)
      (triage-render--star 'situation "Situation" situation))
    (when (and (equal kind "plan") (triage-render--present-p body))
      (triage-render--star 'plan "Plan" body))
    (when (and (triage-render--present-p task)
               (not (equal task (triage-card-title-of card))))
      (triage-render--star 'task "Task" task))
    (when (or (triage-render--present-p result) info-body)
      (magit-insert-section (triage-star 'result)
        (magit-insert-heading "Result")
        (when (triage-render--present-p result)
          (triage-render--insert-markdown result)
          (when info-body (triage-render--gap)))
        (when info-body
          (triage-render--insert-markdown body))
        (triage-render--gap)))))

(defun triage-render--answer (draft k)
  "Return the stored answer slot for question K of DRAFT, or nil."
  (let ((answers (triage-draft-answers draft)))
    (and (vectorp answers) (< k (length answers)) (aref answers k))))

(defun triage-render--answer-label (question slot)
  "Return the label for answer SLOT to QUESTION, or \"Unanswered\"."
  (cond ((null slot) "Unanswered")
        ((alist-get 'text slot) (format "“%s”" (alist-get 'text slot)))
        (t (or (alist-get 'label
                          (nth (alist-get 'option slot) (alist-get 'options question)))
               "Unanswered"))))

(defun triage-render--option-rows (card options step with-direction actions-fn)
  "Insert OPTIONS as buttons, recommended options first.
CARD is the card, STEP the question step or 0, and WITH-DIRECTION adds each
option's direction line.  ACTIONS-FN is called with (pick . INDEX).  The row
that RET picks ends with a RET hint when the card has no lead.  A blank
line separates the options when any of them has a line under it."
  (let* ((rec (triage-card-rec-index options))
         (hint (not (triage-card-lead card step)))
         (lines (lambda (option)
                  (delq nil (list (alist-get 'description option)
                                  (and with-direction (alist-get 'direction option))))))
         (spaced (cl-some lines options)))
    (dolist (entry (triage-card-sorted-options options))
      (let* ((index (car entry))
             (option (cdr entry))
             (key (triage-render--digit index))
             (under (concat triage-render--indent (make-string (1+ (length key)) ?\s))))
        (insert triage-render--indent)
        (triage-render-button (or (alist-get 'label option) "") key nil
                              (funcall actions-fn (cons 'pick index)))
        (when (alist-get 'recommended option)
          (insert "  " (triage-render--face "Recommended" 'triage-recommended)))
        (when (and hint (= index rec))
          (insert "  " (triage-render--face "RET" 'triage-key)))
        (insert "\n")
        (dolist (line (funcall lines option))
          (insert (triage-render--face (concat under line) 'triage-muted) "\n"))
        (when spaced (triage-render--gap))))))

(defun triage-render--other-row (count draft actions-fn)
  "Insert the Other button after COUNT options, then the draft text under it.
DRAFT supplies the text and ACTIONS-FN supplies the `other' action."
  (let ((key (if (< count 9) (number-to-string (1+ count)) "o")))
    (insert triage-render--indent)
    (triage-render-button "Other…" key nil (funcall actions-fn 'other))
    (insert "\n")
    (let ((text (triage-draft-other draft)))
      (when (triage-render--present-p text)
        (insert (triage-render--face
                 (concat triage-render--indent (make-string (1+ (length key)) ?\s) text)
                 'triage-muted)
                "\n")))))

(defun triage-render--readonly-options (card)
  "Insert the options of CARD as muted text, with the note that they are read only."
  (dolist (entry (triage-card-sorted-options (alist-get 'options card)))
    (insert (triage-render--face
             (format "%s%s %s" triage-render--indent (triage-render--digit (car entry))
                     (or (alist-get 'label (cdr entry)) ""))
             'triage-muted)
            "\n"))
  (triage-render--gap)
  (insert triage-render--indent
          (if (equal (alist-get 'engine card) "dsh")
              "Answer in the dsh web UI. This prompt is read only here."
            "Answer in the pane. This prompt is read only here.")
          "\n")
  (triage-render--gap))

(defun triage-render--plain-options (card draft actions-fn)
  "Insert option buttons for CARD without questions.
Add Other when allowed.  DRAFT and ACTIONS-FN follow the card renderer API."
  (let ((options (alist-get 'options card)))
    (triage-render--option-rows card options 0 t actions-fn)
    (when (triage-card-other-allowed-p card 0)
      (triage-render--other-row (length options) draft actions-fn))
    (triage-render--gap)))

(defun triage-render--question-step (card draft actions-fn)
  "Insert the options of the current question of CARD, then Other when allowed.
DRAFT supplies the step and ACTIONS-FN supplies the actions."
  (let* ((step (triage-draft-step draft))
         (questions (alist-get 'questions card))
         (options (alist-get 'options (nth step questions))))
    (insert triage-render--indent
            (triage-render--face (format "Question %d of %d" (1+ step) (length questions))
                                 'triage-muted)
            "\n")
    (triage-render--gap)
    (triage-render--option-rows card options step nil actions-fn)
    (when (triage-card-other-allowed-p card step)
      (triage-render--other-row (length options) draft actions-fn))
    (triage-render--gap)))

(defun triage-render--review (card draft actions-fn)
  "Insert the review step of CARD: one row per answer and the send button.
DRAFT supplies the answers and ACTIONS-FN supplies the actions."
  (let ((complete t))
    (cl-loop for question in (alist-get 'questions card)
             for k from 0
             do (let ((slot (triage-render--answer draft k)))
                  (unless slot (setq complete nil))
                  (insert triage-render--indent)
                  (triage-render-button
                   (triage-render--answer-label question slot)
                   (triage-render--digit k) nil
                   (funcall actions-fn (cons 'step k)))
                  (insert (triage-render--face
                           (format "  %s (Change)" (triage-render--question-text question))
                           'triage-muted)
                          "\n")))
    (triage-render--gap)
    (insert triage-render--indent)
    (triage-render-button (if (equal (alist-get 'kind card) "finding")
                              "Dispatch answers"
                            "Send answers")
                          "RET"
                          (if complete 'triage-primary 'triage-muted)
                          (funcall actions-fn 'send-answers))
    (insert "\n")
    (triage-render--gap)))

(defun triage-render--execution (card draft actions-fn)
  "Insert the execution model and, for dsh, effort lines of a live plan CARD.
DRAFT supplies the chosen values and ACTIONS-FN supplies the actions."
  (let* ((dsh (equal (alist-get 'engine card) "dsh"))
         (executor (triage-draft-executor draft))
         (thinking (triage-draft-executor-thinking draft))
         (has-executor (triage-render--present-p executor)))
    (insert triage-render--indent)
    (triage-render-button
     (format "Execution model: %s"
             (cond (has-executor executor)
                   (dsh "Default (current model)")
                   (t "Default (current Plan Review role)")))
     "e" nil (funcall actions-fn 'executor))
    (insert "\n")
    (when dsh
      (insert triage-render--indent)
      (triage-render-button
       (format "Execution effort: %s"
               (cond ((triage-render--present-p thinking) thinking)
                     (has-executor "Default (Planner effort)")
                     (t "Default (needs an execution model)")))
       "E" nil (funcall actions-fn 'effort))
      (insert "\n"))
    (triage-render--gap)))

(defun triage-render--note (draft actions-fn)
  "Insert the note line for DRAFT, a button that edits the note via ACTIONS-FN."
  (let ((note (triage-draft-note draft)))
    (insert triage-render--indent)
    (triage-render-button
     (format "Note: %s"
             (if (triage-render--present-p note)
                 (string-replace "\n" " ⏎ " note)
               "(none)"))
     "n" nil (funcall actions-fn 'note))
    (insert "\n")
    (triage-render--gap)))

(defun triage-render--action (card draft actions-fn star)
  "Insert the Action section of CARD: its action, options and note.
DRAFT supplies its text, ACTIONS-FN supplies actions and STAR is active."
  (let* ((kind (alist-get 'kind card))
         (questions (alist-get 'questions card))
         (asking (and questions (triage-card-answerable-p card)))
         (action (alist-get 'action star)))
    (magit-insert-section (triage-star 'action)
      (magit-insert-heading "Action")
      (when (triage-render--present-p action)
        (triage-render--insert-markdown action)
        (triage-render--gap))
      (cond
       ((equal kind "info"))
       ((not (triage-card-answerable-p card))
        (triage-render--readonly-options card))
       ((and asking (< (triage-draft-step draft) (length questions)))
        (triage-render--question-step card draft actions-fn))
       (asking (triage-render--review card draft actions-fn))
       (t
        (triage-render--plain-options card draft actions-fn)
        (when (equal kind "plan")
          (triage-render--execution card draft actions-fn))))
      (triage-render--note draft actions-fn))))

(defun triage-render--separate (columns separator)
  "Insert SEPARATOR before the next item of a line, COLUMNS wide.
Start a new line instead when the item would pass the text width."
  (insert (if (> (+ (current-column) (string-width separator) columns)
                 (triage-render--width))
              "\n"
            separator)))

(defun triage-render--dock (card step actions-fn)
  "Insert CARD's action dock at STEP.  ACTIONS-FN supplies button callbacks.
The lead action comes first with RET.  Other actions follow in table order,
wrapped between buttons to the width of the text."
  (let* ((buttons
          (delq nil
                (mapcar (lambda (entry)
                          (let ((label (triage-card-label card (car entry) step)))
                            (and label (cons (car entry) label))))
                        triage-card-actions)))
         (lead (triage-card-lead card step))
         (lead-button (assq lead buttons))
         (ordered (if lead-button (cons lead-button (remq lead-button buttons)) buttons)))
    (dolist (button ordered)
      (let* ((id (car button))
             (leadp (eq id lead))
             (key (concat (plist-get (cdr (assq id triage-card-actions)) :key)
                          (and leadp "/RET"))))
        (unless (bolp)
          (triage-render--separate (string-width (concat key " " (cdr button))) "   "))
        (triage-render-button (cdr button) key (and leadp 'triage-primary)
                              (funcall actions-fn id))))
    (unless (bolp)
      (triage-render--separate 6 "   "))
    (insert (triage-render--face "?" 'triage-key) " "
            (triage-render--face "more" 'triage-muted) "\n")))

(defun triage-render-card (card draft actions-fn)
  "Insert CARD as one `triage-card' section, shaped by DRAFT.
ACTIONS-FN is called with an action id from `triage-card-actions', or with
\(pick . INDEX) for an option, (step . INDEX) for a review row, or the symbol
`send-answers'.  It returns a zero-argument function that runs the action."
  (let* ((questions (alist-get 'questions card))
         (asking (and questions (triage-card-answerable-p card)))
         (step (if asking (triage-draft-step draft) 0))
         (question (and asking (< step (length questions)) (nth step questions)))
         (star (or (alist-get 'star question) (alist-get 'star card)))
         (status (triage-card-status card)))
    (magit-insert-section (triage-card (alist-get 'id card))
      (insert (triage-render--face (plist-get status :label)
                                   (triage-render--tone-face (plist-get status :tone)))
              "\n")
      (insert (triage-render--face
               (or (triage-render--title card asking step question status) "")
               'triage-title)
              "\n")
      (triage-render--facts card)
      (triage-render--stars card star)
      (triage-render--action card draft actions-fn star)
      (triage-render--dock card (if asking step 0) actions-fn))))

(defun triage-render-context-item (card swipes anchor-p in-session-p)
  "Insert CARD as one context item, given its SWIPES.
ANCHOR-P marks the card the context was opened from, which starts expanded.
IN-SESSION-P prefixes the heading with a bar.  A superseded card is muted."
  (let* ((id (alist-get 'id card))
         (swipe (car (last swipes)))
         (superseded (equal (alist-get 'reason swipe) "superseded"))
         (created (alist-get 'created_ms card))
         (heading (format "%s%s · %s · %s · %s%s"
                          (if in-session-p "┃ " "")
                          (alist-get 'kind card)
                          (plist-get (triage-card-status card) :label)
                          (triage-card-source-name (alist-get 'source card))
                          (if created (triage-card-clock created (float-time)) "Time unknown")
                          (if anchor-p " · This card" "")))
         (title (alist-get 'title card))
         (details (and swipe (triage-card-swipe-details card swipe)))
         (result (if swipe
                     (concat (triage-card-swipe-status swipe "queued")
                             (and (triage-render--present-p details)
                                  (concat " · " details)))
                   "queued"))
         (body (alist-get 'body card)))
    (magit-insert-section (triage-context-item id (not anchor-p))
      (magit-insert-heading
        (triage-render--face heading (and superseded 'triage-muted)))
      (insert triage-render--indent
              (if (triage-render--present-p title) title "Untitled card") "\n"
              triage-render--indent (triage-render--face result 'triage-muted) "\n")
      (when (triage-render--present-p body)
        (triage-render--gap)
        (magit-insert-section (triage-context-report id (not anchor-p))
          (magit-insert-heading (concat triage-render--indent "Report"))
          (triage-render--insert-markdown
           body (concat triage-render--indent triage-render--indent))))
      (triage-render--gap))))

(provide 'triage-render)
;;; triage-render.el ends here
