;;; triage-launch.el --- Dispatch and New session sheets for triage -*- lexical-binding: t; -*-

;;; Commentary:

;; The Dispatch and New session sheets.  Each is an edit buffer.  Its
;; read-only preamble shows the options.  The body below it is the
;; instruction or the prompt.  `C-c C-c' launches, `C-c C-k' cancels and
;; `C-c C-o' opens the option menu.  The functions here port openDispatch,
;; submitDispatch, openCompose and submitCompose from ui/app.js.

;;; Code:

(require 'cl-lib)
(require 'seq)
(require 'subr-x)
(require 'transient)
(require 'url-util)
(require 'triage-api)
(require 'triage-card)
(require 'triage-queue)
(require 'triage-render)

(defconst triage-launch--dispatch-buffer "*triage dispatch*"
  "Name of the Dispatch sheet buffer.  One sheet serves every card.")

(defconst triage-launch--compose-buffer "*triage new session*"
  "Name of the New session sheet buffer.")

(defconst triage-launch--max-image-bytes 10485760
  "Largest image the daemon accepts, in decoded bytes.")

(defconst triage-launch--image-types
  '(("png" . "image/png") ("jpg" . "image/jpeg") ("jpeg" . "image/jpeg")
    ("webp" . "image/webp") ("gif" . "image/gif"))
  "Image file extensions mapped to the media types the daemon accepts.")

(defvar triage-launch--daemon-engine nil
  "Engine the daemon runs by default, learned from its model catalog.")

(defvar triage-launch--last-dispatch-engine nil
  "Engine last chosen in the Dispatch sheet, or nil for Default.")

(defvar triage-launch--last-compose-mode "goal"
  "Mode last chosen in the New session sheet.")

(defvar triage-launch--active nil
  "Launch sheet that the option menu acts on.")

(defvar-local triage-launch--kind nil
  "Kind of this sheet: `dispatch' or `compose'.")

(defvar-local triage-launch--card-id nil
  "Id of the card a Dispatch sheet launches.")

(defvar-local triage-launch--mode nil
  "Wire name of the mode the sheet shows.")

(defvar-local triage-launch--engine nil
  "Engine the sheet launches on, or nil for the daemon's engine.")

(defvar-local triage-launch--model nil
  "Model selector the sheet launches with, or nil for Default.")

(defvar-local triage-launch--thinking nil
  "Thinking level the sheet launches with, or nil for Default.")

(defvar-local triage-launch--starting nil
  "Non-nil while this New session sheet has a launch request in flight.")

(defun triage-launch--compose-starting-p ()
  "Return non-nil when the New session sheet has a launch in flight."
  (let ((buffer (get-buffer triage-launch--compose-buffer)))
    (and buffer (buffer-local-value 'triage-launch--starting buffer))))

(defvar-local triage-launch--executor nil
  "Execution model selector for plan-into, or nil for Default.")

(defvar-local triage-launch--executor-thinking nil
  "Execution effort for plan-into, or nil for Default.")

(defvar-local triage-launch--cwd nil
  "Working directory of a New session, or nil for the home directory.")

(defvar-local triage-launch--session nil
  "Tmux session for a New session, or nil for the default session.")

(defvar-local triage-launch--image nil
  "Image plist (:media_type :data :name) attached to the sheet, or nil.")

(defvar-local triage-launch--option nil
  "Index of the option a Dispatch sheet answers with, or nil.")

(defvar-local triage-launch--answers nil
  "Answers to a finding's questions that a Dispatch sheet sends, or nil.")

(defvar-local triage-launch--new-window nil
  "Non-nil when a Dispatch sheet asks for a tmux window of its own.")

(defvar-local triage-launch--default-text nil
  "Default instruction last inserted into the body, or nil.")

(defvar-local triage-launch--body-start nil
  "Marker at the start of the editable body, after the preamble.")

(defvar-local triage-launch--catalogs nil
  "Alist from engine name to the model catalog loaded in this sheet.")

(defvar-local triage-launch--catalog-waiters nil
  "Alist from request key to callbacks waiting for that catalog.")

(defvar-local triage-launch--generation 0
  "Generation of this sheet, incremented when it opens another task.")

(defvar-local triage-launch--tmux-sessions nil
  "Tmux session names that a New session offers.")

(defvar-local triage-launch--tmux-default nil
  "Name of the default tmux session.")

(defvar-local triage-launch--directories nil
  "Working directories that a New session offers.")

(defmacro triage-launch--in-sheet (&rest body)
  "Run BODY in the launch sheet that `triage-launch-options' opened."
  (declare (indent 0) (debug t))
  `(with-current-buffer triage-launch--active ,@body))

(defun triage-launch--nonempty (value)
  "Return VALUE trimmed when it is a non-empty string, else nil."
  (let ((text (and (stringp value) (string-trim value))))
    (and text (not (string-empty-p text)) text)))

(defun triage-launch--or-null (value)
  "Return VALUE trimmed when non-empty, else `:null' for the JSON body."
  (or (triage-launch--nonempty value) :null))

(defun triage-launch--default-label (name)
  "Return the Default row label that names NAME, or plain Default when nil."
  (if name (format "Default (%s)" name) "Default"))

(defun triage-launch--rows ()
  "Return the mode rows of this sheet, each (WIRE LABEL HELP)."
  (if (eq triage-launch--kind 'compose)
      triage-card-compose-modes
    triage-card-dispatch-modes))

(defun triage-launch--compose-mode-name (mode)
  "Return MODE when it is a valid New session wire mode."
  (and (stringp mode) (assoc mode triage-card-compose-modes) mode))

(defun triage-launch--mode-label ()
  "Return the label of the current mode."
  (nth 1 (assoc triage-launch--mode (triage-launch--rows))))

(defun triage-launch--card-title ()
  "Return the title of the Dispatch sheet's card, or its id when it left the queue."
  (let ((card (and triage-launch--card-id (triage-queue-find triage-launch--card-id))))
    (if card (triage-card-title-of card) (or triage-launch--card-id ""))))

(defun triage-launch--body ()
  "Return the body text of the sheet, without the preamble."
  (buffer-substring-no-properties triage-launch--body-start (point-max)))

(defun triage-launch--set-body (text)
  "Replace the body of the sheet with TEXT and leave point at its start."
  (let ((inhibit-read-only t))
    (delete-region triage-launch--body-start (point-max))
    (goto-char triage-launch--body-start)
    (insert text)))

(defun triage-launch--defaulted-p (body)
  "Return non-nil when BODY is the default instruction the sheet inserted."
  (and triage-launch--default-text (string= body triage-launch--default-text)))

(defun triage-launch--cached-catalog (engine)
  "Return the cached catalog for ENGINE, or the daemon's when ENGINE is nil."
  (cdr (assoc (or engine triage-launch--daemon-engine) triage-launch--catalogs)))

(defun triage-launch--with-catalog (engine fn)
  "Call FN with the model catalog for ENGINE, loading it asynchronously if needed."
  (if-let* ((cached (triage-launch--cached-catalog engine)))
      (funcall fn cached)
    (let* ((buffer (current-buffer))
           (generation triage-launch--generation)
           (key (cons generation (or engine "")))
           (pending (assoc key triage-launch--catalog-waiters)))
      (message "Loading models…")
      (if pending
          (setcdr pending (cons fn (cdr pending)))
        (push (cons key (list fn)) triage-launch--catalog-waiters)
        (triage-api-request
         'get (if engine
                  (format "/v1/models?engine=%s" (url-hexify-string engine))
                "/v1/models")
         :then
         (lambda (catalog)
           (when (buffer-live-p buffer)
             (with-current-buffer buffer
               (when (= generation triage-launch--generation)
                 (unless engine
                   (setq triage-launch--daemon-engine (alist-get 'engine catalog)))
                 (push (cons (or engine (alist-get 'engine catalog)) catalog)
                       triage-launch--catalogs)
                 (let ((callbacks (cdr (assoc key triage-launch--catalog-waiters))))
                   (setq triage-launch--catalog-waiters
                         (delq (assoc key triage-launch--catalog-waiters)
                               triage-launch--catalog-waiters))
                   (dolist (callback (reverse callbacks))
                     (funcall callback catalog)))))))
         :else
         (lambda (_status text)
           (when (buffer-live-p buffer)
             (with-current-buffer buffer
               (when (= generation triage-launch--generation)
                 (setq triage-launch--catalog-waiters
                       (delq (assoc key triage-launch--catalog-waiters)
                             triage-launch--catalog-waiters))
                 (message "%s"
                          (propertize (format "Could not load models: %s" text)
                                      'face 'error)))))))))))

(defun triage-launch--effective-engine ()
  "Return the engine this sheet launches on: its own choice, else the daemon's."
  (or triage-launch--engine triage-launch--daemon-engine))

(defun triage-launch--dsh-p ()
  "Return non-nil when this sheet launches on dsh."
  (equal (triage-launch--effective-engine) "dsh"))

(defun triage-launch--engine-name ()
  "Return the effective engine name, or `daemon' when the daemon is unreachable."
  (or (triage-launch--effective-engine) "daemon"))

(defun triage-launch--find-model (catalog selector)
  "Return the model in CATALOG whose selector is SELECTOR, or nil."
  (and selector
       (seq-find (lambda (model) (equal (alist-get 'selector model) selector))
                 (alist-get 'models catalog))))

(defun triage-launch--model-fallback (catalog)
  "Return the Default model name in CATALOG, or the selector."
  (let* ((default (alist-get 'default catalog))
         (model (triage-launch--find-model catalog default)))
    (cond (model (alist-get 'name model))
          (default default)
          (t (format "%s default" (triage-launch--engine-name))))))

(defun triage-launch--model-default ()
  "Return the Default row label for the model."
  (triage-launch--default-label
   (triage-launch--model-fallback (triage-launch--cached-catalog triage-launch--engine))))

(defun triage-launch--thinking-default ()
  "Return the Default row label for the thinking level."
  (let ((catalog (triage-launch--cached-catalog triage-launch--engine)))
    (triage-launch--default-label
     (or (alist-get 'default_thinking catalog)
         (format "%s default" (triage-launch--engine-name))))))

(defun triage-launch--executor-default ()
  "Return the Default row label for the execution model."
  (triage-launch--default-label
   (if (triage-launch--dsh-p) "planner model" "omp smol role")))

(defun triage-launch--executor-effort-default ()
  "Return the Default row label for the execution effort."
  (cond ((not (triage-launch--dsh-p)) "Default (Selector level)")
        (triage-launch--executor "Default (Planner effort)")
        (t "Default (needs an execution model)")))

(defun triage-launch--engine-text ()
  "Return the value shown in the Engine row."
  (or triage-launch--engine
      (triage-launch--default-label (triage-launch--effective-engine))))

(defun triage-launch--model-text ()
  "Return the value shown in the Model row."
  (or triage-launch--model (triage-launch--model-default)))

(defun triage-launch--thinking-text ()
  "Return the value shown in the Thinking row."
  (or triage-launch--thinking (triage-launch--thinking-default)))

(defun triage-launch--executor-text ()
  "Return the value shown in the Exec model row."
  (or triage-launch--executor (triage-launch--executor-default)))

(defun triage-launch--executor-effort-text ()
  "Return the value shown in the Exec effort row."
  (or triage-launch--executor-thinking (triage-launch--executor-effort-default)))

(defun triage-launch--directory-text ()
  "Return the value shown in the Directory row."
  (or triage-launch--cwd "Default (Home directory)"))

(defun triage-launch--session-text ()
  "Return the value shown in the tmux session row."
  (or triage-launch--session
      (triage-launch--default-label triage-launch--tmux-default)))

(defun triage-launch--image-text ()
  "Return the value shown in the Image row."
  (if triage-launch--image (plist-get triage-launch--image :name) "none"))

(defun triage-launch--row (label value)
  "Return a preamble row with LABEL in a muted column before VALUE.
A long VALUE wraps under its own start."
  (propertize (concat (propertize (format "%-15s" label) 'face 'triage-muted) value)
              'wrap-prefix (make-string 15 ?\s)))

(defun triage-launch--preamble ()
  "Return the read-only preamble that sits above the body."
  (let ((compose (eq triage-launch--kind 'compose))
        (plan-into (equal triage-launch--mode "plan-into")))
    (concat
     (mapconcat
      #'identity
      (delq nil
            (list
             (propertize (if compose
                             "New session · What should the session do?"
                           (format "Dispatch · %s" (triage-launch--card-title)))
                         'face 'bold)
             ""
             (triage-launch--row "Mode" (format "%s · %s" (triage-launch--mode-label)
                                                (nth 2 (assoc triage-launch--mode
                                                              (triage-launch--rows)))))
             (and compose (triage-launch--row "Directory" (triage-launch--directory-text)))
             (and compose (not (triage-launch--dsh-p))
                  (triage-launch--row "tmux session" (triage-launch--session-text)))
             (triage-launch--row "Engine" (triage-launch--engine-text))
             (triage-launch--row "Model" (triage-launch--model-text))
             (triage-launch--row "Thinking" (triage-launch--thinking-text))
             (and plan-into (triage-launch--row "Exec model" (triage-launch--executor-text)))
             (and plan-into (triage-launch--row "Exec effort"
                                                (triage-launch--executor-effort-text)))
             (triage-launch--row "Image" (triage-launch--image-text))
             ""
             (mapconcat (lambda (hint)
                          (concat (propertize (car hint) 'face 'triage-key) " " (cdr hint)))
                        '(("C-c C-c" . "launch") ("C-c C-o" . "options") ("C-c C-k" . "cancel"))
                        "   ")
             ""
             (propertize (if compose "──── Prompt ────" "──── Instruction ────")
                         'face 'shadow)))
      "\n")
     "\n")))

(defun triage-launch--draw ()
  "Redraw the read-only preamble above the body of this sheet."
  (let ((inhibit-read-only t)
        (text (triage-launch--preamble)))
    (add-text-properties
     0 (length text)
     '(read-only t front-sticky (read-only) rear-nonsticky (read-only))
     text)
    (save-excursion
      (delete-region (point-min) triage-launch--body-start)
      (set-marker-insertion-type triage-launch--body-start t)
      (goto-char (point-min))
      (insert text)
      (set-marker-insertion-type triage-launch--body-start nil))))

(defun triage-launch--set-mode (mode)
  "Make MODE the mode of this sheet and fill in its default instruction.
Port of setDispatchMode in ui/app.js."
  (when (and triage-launch--answers (member mode '("investigate" "investigate-fix")))
    (setq mode "act"))
  (setq triage-launch--mode mode)
  (let* ((body (triage-launch--body))
         (defaulted (triage-launch--defaulted-p body))
         (fallback (pcase mode
                     ("act" triage-card-act-default)
                     ("investigate-fix" triage-card-fix-default))))
    (cond ((and fallback (or (string-empty-p (string-trim body)) defaulted))
           (triage-launch--set-body fallback)
           (setq triage-launch--default-text fallback))
          ((and (not fallback) defaulted)
           (triage-launch--set-body "")
           (setq triage-launch--default-text nil))))
  (message "%s" (nth 2 (assoc mode (triage-launch--rows)))))

(defun triage-launch--remember (card)
  "Keep this sheet's body and executor choices in the draft of CARD.
The body is not kept while it is the default instruction."
  (let ((draft (triage-queue-draft card)))
    (unless (triage-launch--defaulted-p (triage-launch--body))
      (setf (triage-draft-note draft) (triage-launch--body)))
    (setf (triage-draft-executor draft) (or triage-launch--executor "")
          (triage-draft-executor-thinking draft) (or triage-launch--executor-thinking ""))))

(defun triage-launch--remember-current ()
  "Keep this Dispatch sheet's text in its card's draft, when the card is queued."
  (let ((card (and triage-launch--card-id (triage-queue-find triage-launch--card-id))))
    (when card
      (triage-launch--remember card))))

(defun triage-launch--check-image-size (bytes)
  "Signal `user-error' when an image of BYTES bytes exceeds the limit."
  (when (> bytes triage-launch--max-image-bytes)
    (user-error "That image is over 10 MB")))

(defun triage-launch--read-base64 (file)
  "Return the base64 text of FILE, without line breaks."
  (condition-case nil
      (with-temp-buffer
        (set-buffer-multibyte nil)
        (insert-file-contents-literally file)
        (base64-encode-region (point-min) (point-max) t)
        (buffer-string))
    (file-error (user-error "Could not read the image"))))

(defun triage-launch--file-image (file)
  "Return the image plist for FILE, or signal `user-error'."
  (let* ((extension (downcase (or (file-name-extension file) "")))
         (media-type (cdr (assoc extension triage-launch--image-types)))
         (attributes (file-attributes file)))
    (unless media-type
      (user-error "Attach a PNG, JPEG, WebP or GIF image"))
    (when attributes
      (triage-launch--check-image-size (file-attribute-size attributes)))
    (list :media_type media-type
          :data (triage-launch--read-base64 file)
          :name (file-name-nondirectory file))))

(defun triage-launch--image-json (image)
  "Return the JSON object for IMAGE, a plist from `triage-launch--file-image'."
  `((media_type . ,(plist-get image :media_type))
    (data . ,(plist-get image :data))
    (name . ,(plist-get image :name))))

(defun triage-launch--read-model (prompt default catalog)
  "Read a model selector for PROMPT over DEFAULT and the models in CATALOG.
Return nil when DEFAULT is chosen.  Free text is accepted and trimmed."
  (let* ((models (mapcar (lambda (model)
                           (cons (alist-get 'selector model) (alist-get 'name model)))
                         (alist-get 'models catalog)))
         (completion-extra-properties
          (list :annotation-function
                (lambda (candidate)
                  (let ((name (cdr (assoc candidate models))))
                    (and name (format "  %s" name))))))
         (choice (completing-read prompt (cons default (mapcar #'car models)) nil nil)))
    (unless (equal choice default)
      (triage-launch--nonempty choice))))

(defun triage-launch--read-fixed (prompt rows)
  "Read a choice from ROWS through PROMPT with one mnemonic key.
Each row is (LABEL VALUE) or (LABEL VALUE HELP).  Every label gets its
key from its first letters, the way Magit's dispatch menus key their
commands, and PROMPT shows each key.  HELP reaches the `?' help.
Return the VALUE of the row the picked key names."
  (let* ((keys (triage-card-choice-keys (mapcar #'car rows)))
         (picked (read-multiple-choice
                  prompt
                  (cl-mapcar (lambda (key row)
                               (if (nth 2 row)
                                   (list key (car row) (nth 2 row))
                                 (list key (car row))))
                             keys rows))))
    (nth 1 (nth (cl-position (car picked) keys) rows))))

(defun triage-launch--read-level (prompt default levels)
  "Read a thinking level for PROMPT from DEFAULT and LEVELS.
Return nil when DEFAULT is chosen."
  (triage-launch--read-fixed
   prompt
   (cons (list default nil)
         (mapcar (lambda (level) (list level level)) levels))))

(defun triage-launch--launch-fields (card)
  "Return the launch fields that a Dispatch request sends for CARD."
  (let ((note (string-trim (triage-draft-note (triage-queue-draft card)))))
    (append
     `((mode . ,triage-launch--mode)
       (note . ,(triage-launch--or-null note))
       (engine . ,(triage-launch--or-null triage-launch--engine))
       (model . ,(triage-launch--or-null triage-launch--model))
       (thinking . ,(triage-launch--or-null triage-launch--thinking)))
     (when triage-launch--image
       `((image . ,(triage-launch--image-json triage-launch--image))))
     (when (equal triage-launch--mode "plan-into")
       `((executor . ,(triage-launch--or-null triage-launch--executor))
         (executor_thinking . ,(triage-launch--or-null triage-launch--executor-thinking)))))))

(defun triage-launch--submit-dispatch ()
  "Launch the Dispatch sheet's card.  Port of submitDispatch in ui/app.js."
  (let ((card (triage-queue-find triage-launch--card-id)))
    (if (not card)
        (message "This card is no longer in the queue.")
      (triage-launch--remember card)
      (let* ((launch (triage-launch--launch-fields card))
             (answers triage-launch--answers)
             (option triage-launch--option)
             (done
              (cond
               (answers
                (triage-queue-act
                 card "answer"
                 (append `((answers . ,(vconcat (mapcar (lambda (answer) (or answer :null))
                                                        answers))))
                         launch)))
               (option
                (triage-queue-act
                 card "answer"
                 (append `((option . ,option) (text . :null)) launch)))
               (t
                (triage-queue-act
                 card "dispatch"
                 (append launch
                         `((new_window . ,(if triage-launch--new-window t :false)))))))))
        (when done
          (kill-buffer (current-buffer)))))))

(cl-defun triage-start-session (prompt &key cwd session engine model thinking
                                      (mode "goal") executor executor-thinking
                                      image then else)
  "Start a session through the daemon and return its request process.
PROMPT is the task.  CWD, SESSION, ENGINE, MODEL and THINKING set launch
options.
MODE, EXECUTOR, EXECUTOR-THINKING and IMAGE match `/v1/sessions'.
THEN receives a successful response and ELSE receives status and message."
  (when (string-empty-p (string-trim prompt))
    (user-error "Type what the session should do"))
  (unless (triage-launch--compose-mode-name mode)
    (user-error "Invalid New session mode: %S" mode))
  (triage-api-request
   'post "/v1/sessions"
   :body (append
          `((prompt . ,prompt)
            (cwd . ,(triage-launch--or-null cwd))
            (session . ,(triage-launch--or-null session))
            (engine . ,(triage-launch--or-null engine))
            (model . ,(triage-launch--or-null model))
            (thinking . ,(triage-launch--or-null thinking))
            (mode . ,mode))
          (when image
            `((image . ,(triage-launch--image-json image))))
          (when (equal mode "plan-into")
            `((executor . ,(triage-launch--or-null executor))
              (executor_thinking . ,(triage-launch--or-null executor-thinking)))))
   :then then
   :else else))

(defun triage-launch--submit-compose ()
  "Start the New session that the sheet describes.
Port of submitCompose in ui/app.js."
  (when triage-launch--starting
    (user-error "Session is already starting"))
  (let* ((prompt (string-trim (triage-launch--body)))
         (mode (or (triage-launch--compose-mode-name triage-launch--mode) "goal"))
         (engine (triage-launch--effective-engine))
         (dsh (equal engine "dsh"))
         (buffer (current-buffer)))
    (when (string-empty-p prompt)
      (user-error "Type what the session should do"))
    (setq triage-launch--mode mode
          triage-launch--last-compose-mode mode
          triage-launch--starting t)
    (message "Starting…")
    (condition-case err
        (triage-start-session
         prompt
         :cwd triage-launch--cwd
         :session triage-launch--session
         :engine triage-launch--engine
         :model triage-launch--model
         :thinking triage-launch--thinking
         :mode mode
         :executor triage-launch--executor
         :executor-thinking triage-launch--executor-thinking
         :image triage-launch--image
         :then (lambda (response)
                 (when (buffer-live-p buffer)
                   (kill-buffer buffer))
                 (message "%s"
                          (cond ((alist-get 'starting response)
                                 (if dsh "Starting a dsh session." "Starting the session."))
                                (dsh "Started a dsh session.")
                                (t (format "Started · %s" (alist-get 'pane response))))))
         :else (lambda (_status message)
                 (when (buffer-live-p buffer)
                   (with-current-buffer buffer
                     (setq triage-launch--starting nil)))
                 (message "%s" (propertize message 'face 'error))))
      (error
       (setq triage-launch--starting nil)
       (signal (car err) (cdr err))))))

(defun triage-launch-submit ()
  "Launch the Dispatch sheet's card, or start the New session the sheet describes."
  (interactive)
  (if (eq triage-launch--kind 'compose)
      (triage-launch--submit-compose)
    (triage-launch--submit-dispatch)))

(defun triage-launch-cancel ()
  "Close the sheet without launching.
A Dispatch sheet keeps its text in its card's draft."
  (interactive)
  (when triage-launch--starting
    (user-error "Wait for the session to finish starting"))
  (when (eq triage-launch--kind 'dispatch)
    (triage-launch--remember-current))
  (kill-buffer (current-buffer)))

(cl-defun triage-launch-dispatch (card mode &key new-window option answers)
  "Open the Dispatch sheet for CARD in MODE and show it.
NEW-WINDOW asks for a tmux window of its own.  OPTION is the index of the
option that the card's answer picks.  ANSWERS is the vector of answers to a
finding's questions.  Port of openDispatch in ui/app.js."
  (let ((buffer (get-buffer-create triage-launch--dispatch-buffer)))
    (with-current-buffer buffer
      (triage-launch--remember-current)
      (unless (derived-mode-p 'triage-launch-mode)
        (triage-launch-mode))
      (let* ((engine (cond ((triage-card-dsh-id card) "dsh")
                           ((alist-get 'pane (alist-get 'source card)) "omp")
                           (t triage-launch--last-dispatch-engine)))
             (draft (triage-queue-draft card)))
        (unless (equal triage-launch--engine engine)
          (setq triage-launch--model nil
                triage-launch--thinking nil))
        (setq triage-launch--kind 'dispatch
              triage-launch--card-id (alist-get 'id card)
              triage-launch--engine engine
              triage-launch--option option
              triage-launch--answers answers
              triage-launch--new-window new-window
              triage-launch--executor (triage-launch--nonempty (triage-draft-executor draft))
              triage-launch--executor-thinking
              (triage-launch--nonempty (triage-draft-executor-thinking draft))
              triage-launch--default-text nil
              triage-launch--catalogs nil
              triage-launch--catalog-waiters nil
              triage-launch--generation (1+ triage-launch--generation))
        (triage-launch--set-body (triage-draft-note draft))
        (triage-launch--set-mode mode)
        (triage-launch--draw)
        (triage-launch--with-catalog engine (lambda (_catalog) (triage-launch--draw)))
        (goto-char (point-max))))
    (pop-to-buffer buffer)))

(defun triage-launch--load-tmux-sessions ()
  "Load the tmux sessions and directories for a New session.
A failed request leaves the lists empty."
  (let ((data (condition-case nil
                  (triage-api-request-sync 'get "/v1/tmux-sessions")
                (triage-api-error nil))))
    (setq triage-launch--tmux-sessions (alist-get 'sessions data)
          triage-launch--tmux-default (alist-get 'default data)
          triage-launch--directories (alist-get 'directories data))))

;;;###autoload
(defun triage-new-session (&optional prompt cwd engine)
  "Open the New session sheet.  Port of openCompose in ui/app.js.
When PROMPT is non-nil, fill the body and use CWD and ENGINE for the launch."
  (interactive)
  (let ((buffer (get-buffer-create triage-launch--compose-buffer)))
    (when (triage-launch--compose-starting-p)
      (user-error "Wait for the current session to finish starting"))
    (with-current-buffer buffer
      (unless (derived-mode-p 'triage-launch-mode)
        (triage-launch-mode))
      (when prompt
        (let ((draft (string-trim (triage-launch--body))))
          (when (and (not (string-empty-p draft))
                     (not (equal draft (string-trim prompt)))
                     (not (y-or-n-p "Replace the New session draft? ")))
            (user-error "New session draft kept"))))
      (setq triage-launch--kind 'compose
            triage-launch--card-id nil
            triage-launch--option nil
            triage-launch--answers nil
            triage-launch--new-window nil
            triage-launch--mode
            (or (triage-launch--compose-mode-name triage-launch--last-compose-mode)
                "goal")
            triage-launch--last-compose-mode triage-launch--mode
            triage-launch--starting nil
            triage-launch--default-text nil
            triage-launch--catalogs nil
            triage-launch--catalog-waiters nil
            triage-launch--generation (1+ triage-launch--generation))
      (when prompt
        (setq triage-launch--cwd cwd
              triage-launch--engine engine
              triage-launch--model nil
              triage-launch--thinking nil
              triage-launch--session nil
              triage-launch--image nil
              triage-launch--executor nil
              triage-launch--executor-thinking nil)
        (triage-launch--set-body prompt)
        (goto-char (point-max)))
      (triage-launch--load-tmux-sessions)
      (triage-launch--draw)
      (triage-launch--with-catalog triage-launch--engine
                                   (lambda (_catalog) (triage-launch--draw))))
    (pop-to-buffer buffer)))

;;;###autoload
(defun triage-model-selectors (engine)
  "Return the daemon's model selectors for ENGINE."
  (delq nil
        (mapcar (lambda (model) (alist-get 'selector model))
                (alist-get 'models
                           (triage-api-request-sync
                            'get (format "/v1/models?engine=%s"
                                         (url-hexify-string engine)))))))

(defun triage-launch-set-mode ()
  "Choose the mode of the launch sheet."
  (interactive)
  (triage-launch--in-sheet
    (let* ((rows (seq-remove
                  (lambda (row)
                    (and triage-launch--answers
                         (member (car row) '("investigate" "investigate-fix"))))
                  (triage-launch--rows)))
          (mode (triage-launch--read-fixed
                 "Mode"
                 (mapcar (lambda (row) (list (cadr row) (car row) (nth 2 row)))
                         rows))))
      (triage-launch--set-mode mode)
      (when (eq triage-launch--kind 'compose)
        (setq triage-launch--last-compose-mode mode))
      (triage-launch--draw))))

(defun triage-launch-set-engine ()
  "Choose the engine of the launch sheet, or Default."
  (interactive)
  (triage-launch--in-sheet
    (let ((engine (triage-launch--read-fixed
                   "Engine"
                   '(("Default" nil) ("omp" "omp") ("dsh" "dsh")))))
      (setq triage-launch--engine engine
            triage-launch--model nil
            triage-launch--thinking nil
            triage-launch--executor nil
            triage-launch--executor-thinking nil)
      (when (eq triage-launch--kind 'dispatch)
        (setq triage-launch--last-dispatch-engine engine))
      (triage-launch--draw)
      (triage-launch--with-catalog engine (lambda (_catalog) (triage-launch--draw))))))

(defun triage-launch-set-model ()
  "Choose the model of the launch sheet."
  (interactive)
  (triage-launch--in-sheet
    (let ((engine triage-launch--engine))
      (triage-launch--with-catalog
       engine
       (lambda (catalog)
         (when (equal engine triage-launch--engine)
           (let ((model (triage-launch--read-model
                         "Model: " (triage-launch--model-default) catalog)))
             (setq triage-launch--model model)
             (unless (member triage-launch--thinking
                             (triage-card-effort-levels catalog model))
               (setq triage-launch--thinking nil))
             (triage-launch--draw))))))))

(defun triage-launch-set-thinking ()
  "Choose the thinking level of the launch sheet."
  (interactive)
  (triage-launch--in-sheet
    (let ((engine triage-launch--engine))
      (triage-launch--with-catalog
       engine
       (lambda (catalog)
         (when (equal engine triage-launch--engine)
           (let ((choice (triage-launch--read-level
                          "Thinking: " (triage-launch--thinking-default)
                          (triage-card-effort-levels catalog triage-launch--model))))
             (setq triage-launch--thinking choice)
             (triage-launch--draw))))))))

(defun triage-launch-set-executor ()
  "Choose the execution model for plan-into, or Default."
  (interactive)
  (triage-launch--in-sheet
    (let ((engine triage-launch--engine))
      (triage-launch--with-catalog
       engine
       (lambda (catalog)
         (when (equal engine triage-launch--engine)
           (let ((executor (triage-launch--read-model
                            "Execution model: " (triage-launch--executor-default) catalog)))
             (setq triage-launch--executor executor)
             (unless (member triage-launch--executor-thinking
                             (triage-card-effort-levels catalog executor))
               (setq triage-launch--executor-thinking nil))
             (triage-launch--draw))))))))

(defun triage-launch-set-executor-effort ()
  "Choose the execution effort for plan-into."
  (interactive)
  (triage-launch--in-sheet
    (when (and (triage-launch--dsh-p) (not triage-launch--executor))
      (user-error "Choose an execution model first"))
    (let ((engine triage-launch--engine))
      (triage-launch--with-catalog
       engine
       (lambda (catalog)
         (when (equal engine triage-launch--engine)
           (let ((choice (triage-launch--read-level
                          "Execution effort: " (triage-launch--executor-effort-default)
                          (triage-card-effort-levels catalog triage-launch--executor))))
             (setq triage-launch--executor-thinking choice)
             (triage-launch--draw))))))))

(defun triage-launch-set-directory ()
  "Choose the working directory of the New session sheet."
  (interactive)
  (triage-launch--in-sheet
    (let* ((default "Default (Home directory)")
           (choice (completing-read "Directory: " (cons default triage-launch--directories)
                                    nil nil)))
      (setq triage-launch--cwd
            (unless (equal choice default)
              (triage-launch--nonempty choice)))
      (triage-launch--draw))))

(defun triage-launch-set-session ()
  "Choose the tmux session of the New session sheet."
  (interactive)
  (triage-launch--in-sheet
    (let* ((default (triage-launch--default-label triage-launch--tmux-default))
           (choice (completing-read "tmux session: " (cons default triage-launch--tmux-sessions)
                                    nil t)))
      (setq triage-launch--session (unless (equal choice default) choice))
      (triage-launch--draw))))

(defun triage-launch-attach-image ()
  "Attach a PNG, JPEG, WebP or GIF image from a file to the sheet."
  (interactive)
  (triage-launch--in-sheet
    (let ((file (read-file-name "Image: " nil nil t)))
      (setq triage-launch--image (triage-launch--file-image file))
      (triage-launch--draw))))

(defun triage-launch-paste-image ()
  "Attach the PNG image on the clipboard to the sheet."
  (interactive)
  (triage-launch--in-sheet
    (let ((data (and (display-graphic-p)
                     (gui-get-selection 'CLIPBOARD 'image/png))))
      (unless data
        (user-error "The clipboard holds no PNG image"))
      (triage-launch--check-image-size (length data))
      (setq triage-launch--image (list :media_type "image/png"
                                       :data (base64-encode-string data t)
                                       :name "clipboard.png"))
      (triage-launch--draw))))

(defun triage-launch-drop-image ()
  "Remove the attached image from the sheet."
  (interactive)
  (triage-launch--in-sheet
    (setq triage-launch--image nil)
    (triage-launch--draw)))

(defun triage-launch-insert-phrase ()
  "Insert one of the saved dictation phrases at point."
  (interactive)
  (triage-launch--in-sheet
    (let* ((data (triage-api-request-sync 'get "/v1/dictation"))
           (phrases (seq-remove
                     #'string-empty-p
                     (mapcar #'string-trim
                             (split-string (or (alist-get 'phrases data) "") "\n")))))
      (unless phrases
        (user-error "No saved phrases.  Add them with M-x triage-settings"))
      (let ((phrase (completing-read "Phrase: " phrases nil t)))
        (when (< (point) triage-launch--body-start)
          (goto-char (point-max)))
        (insert phrase)))))

(transient-define-prefix triage-launch--options-menu ()
  "Options for the triage launch sheet."
  [["Launch"
    ("m" (lambda ()
           (triage-launch--in-sheet (format "Mode: %s" (triage-launch--mode-label))))
     triage-launch-set-mode)
    ("e" (lambda ()
           (triage-launch--in-sheet (format "Engine: %s" (triage-launch--engine-text))))
     triage-launch-set-engine)
    ("M" (lambda ()
           (triage-launch--in-sheet (format "Model: %s" (triage-launch--model-text))))
     triage-launch-set-model)
    ("t" (lambda ()
           (triage-launch--in-sheet (format "Thinking: %s" (triage-launch--thinking-text))))
     triage-launch-set-thinking)]
   ["Plan into"
    :if (lambda () (triage-launch--in-sheet (equal triage-launch--mode "plan-into")))
    ("x" (lambda ()
           (triage-launch--in-sheet (format "Exec model: %s" (triage-launch--executor-text))))
     triage-launch-set-executor)
    ("X" (lambda ()
           (triage-launch--in-sheet
             (format "Exec effort: %s" (triage-launch--executor-effort-text))))
     triage-launch-set-executor-effort)]
   ["New session"
    :if (lambda () (triage-launch--in-sheet (eq triage-launch--kind 'compose)))
    ("d" (lambda ()
           (triage-launch--in-sheet (format "Directory: %s" (triage-launch--directory-text))))
     triage-launch-set-directory)
    ("s" (lambda ()
           (triage-launch--in-sheet (format "tmux session: %s" (triage-launch--session-text))))
     triage-launch-set-session
     :inapt-if (lambda () (triage-launch--in-sheet (triage-launch--dsh-p))))]
   ["Image"
    ("a" "Attach image" triage-launch-attach-image)
    ("y" "Paste image" triage-launch-paste-image)
    ("D" "Drop image" triage-launch-drop-image
     :inapt-if-not (lambda () (triage-launch--in-sheet triage-launch--image)))]
   ["Text"
    ("p" "Insert phrase" triage-launch-insert-phrase)]])

(defun triage-launch-options ()
  "Open the option menu for the launch sheet in the current buffer."
  (interactive)
  (setq triage-launch--active (current-buffer))
  (transient-setup 'triage-launch--options-menu))

(defvar-keymap triage-launch-mode-map
  :parent text-mode-map
  "C-c C-c" #'triage-launch-submit
  "C-c C-k" #'triage-launch-cancel
  "C-c C-o" #'triage-launch-options)

(define-derived-mode triage-launch-mode text-mode "Triage-Launch"
  "Major mode for the triage Dispatch and New session sheets.
The top of the buffer is a read-only preamble that shows the options.  The
body below it is the instruction or the prompt."
  (triage-render-setup-view)
  ;; The rows carry their own wrap prefix, which this mode would replace.
  (visual-wrap-prefix-mode -1)
  (setq triage-launch--body-start (point-min-marker)))

(provide 'triage-launch)
;;; triage-launch.el ends here
