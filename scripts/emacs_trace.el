;;; emacs_trace.el --- Interactive GNU Emacs behavior recorder -*- lexical-binding: t; -*-

;; This recorder intentionally observes public editor behavior rather than
;; Emacs's internal redisplay machinery.  Load it into a clean Emacs, record a
;; sequence in a read-only fixture buffer, then import the resulting JSON into
;; Herdr's differential conformance corpus.

(require 'json)
(require 'seq)
(require 'subr-x)

(defvar herdr-emacs-trace--session nil
  "The active recorder session, or nil.")

(defvar herdr-emacs-trace--pending nil
  "Stack of command observations created by `pre-command-hook'.")

(defvar herdr-emacs-trace--in-hook nil
  "Non-nil while a recorder hook is taking a snapshot.")

(defvar-local herdr-emacs-trace-mode nil)

(defvar herdr-emacs-trace-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "C-c C-c") #'herdr-emacs-trace-stop)
    map)
  "Keys available in a trace fixture buffer.")

(define-minor-mode herdr-emacs-trace-mode
  "Record completed Emacs commands and their observable state transitions."
  :lighter " Herdr-Trace"
  :keymap herdr-emacs-trace-mode-map
  (unless herdr-emacs-trace-mode
    (when (and herdr-emacs-trace--session
               (eq (current-buffer)
                   (plist-get herdr-emacs-trace--session :buffer)))
      (herdr-emacs-trace-stop))))

(defun herdr-emacs-trace--json-false (value)
  (if value t :false))

(defun herdr-emacs-trace--mark-active-p ()
  ;; `post-command-hook' runs just before the command loop applies
  ;; `deactivate-mark'.  Record the externally observable post-command state,
  ;; not that short-lived implementation detail.
  (and mark-active (not deactivate-mark)))

(defun herdr-emacs-trace--position (position buffer)
  "Return a stable, zero-based position for POSITION in BUFFER."
  (if (not position)
      :null
    (with-current-buffer buffer
      (save-restriction
        (widen)
        (save-excursion
          (goto-char (min (max position (point-min)) (point-max)))
          (let ((result (make-hash-table :test 'equal)))
            (puthash "row" (1- (line-number-at-pos (point) t)) result)
            (puthash "col" (current-column) result)
            (puthash "char_offset" (1- (point)) result)
            (puthash "byte_offset" (1- (position-bytes (point))) result)
            result))))))

(defun herdr-emacs-trace--marker-position (marker buffer)
  (when (and (markerp marker)
             (marker-buffer marker)
             (eq (marker-buffer marker) buffer))
    (herdr-emacs-trace--position (marker-position marker) buffer)))

(defun herdr-emacs-trace--mark-ring (buffer)
  (with-current-buffer buffer
    (vconcat
     (delq nil
           (mapcar (lambda (marker)
                     (herdr-emacs-trace--marker-position marker buffer))
                   mark-ring)))))

(defun herdr-emacs-trace--kill-ring ()
  (vconcat
   (mapcar (lambda (entry) (substring-no-properties entry)) kill-ring)))

(defun herdr-emacs-trace--kill-ring-index ()
  (if (and kill-ring kill-ring-yank-pointer)
      (or (seq-position kill-ring kill-ring-yank-pointer #'eq) 0)
    :null))

(defun herdr-emacs-trace--region (buffer)
  (with-current-buffer buffer
    (if (not (and (herdr-emacs-trace--mark-active-p) (mark t)))
        :null
      (let* ((point-position (point))
             (mark-position (mark t))
             (start (min point-position mark-position))
             (end (max point-position mark-position))
             (result (make-hash-table :test 'equal)))
        (puthash "start" (herdr-emacs-trace--position start buffer) result)
        (puthash "end" (herdr-emacs-trace--position end buffer) result)
        (puthash "direction"
                 (if (< point-position mark-position) "backward" "forward")
                 result)
        (puthash "text"
                 (buffer-substring-no-properties start end)
                 result)
        result))))

(defun herdr-emacs-trace--minibuffer ()
  (let ((window (active-minibuffer-window)))
    (if (not (window-live-p window))
        :null
      (with-current-buffer (window-buffer window)
        (let ((result (make-hash-table :test 'equal)))
          (puthash "prompt"
                   (minibuffer-prompt)
                   result)
          (puthash "contents"
                   (minibuffer-contents-no-properties)
                   result)
          (puthash "point" (- (point) (minibuffer-prompt-end)) result)
          result)))))

(defun herdr-emacs-trace--isearch ()
  (if (not (bound-and-true-p isearch-mode))
      :null
    (let ((result (make-hash-table :test 'equal)))
      (puthash "query" (substring-no-properties isearch-string) result)
      (puthash "direction" (if isearch-forward "forward" "backward") result)
      (puthash "failing" (herdr-emacs-trace--json-false isearch-failing) result)
      result)))

(defun herdr-emacs-trace--buffer-summary (buffer)
  (with-current-buffer buffer
    (let ((result (make-hash-table :test 'equal)))
      (save-restriction
        (widen)
        (puthash "sha256" (secure-hash 'sha256 (current-buffer)) result)
        (puthash "characters" (buffer-size) result)
        (puthash "bytes" (1- (position-bytes (point-max))) result)
        (puthash "lines" (line-number-at-pos (point-max) t) result))
      (puthash "modified" (herdr-emacs-trace--json-false (buffer-modified-p)) result)
      (puthash "read_only" (herdr-emacs-trace--json-false buffer-read-only) result)
      (puthash "major_mode" (symbol-name major-mode) result)
      (puthash "multibyte" (herdr-emacs-trace--json-false enable-multibyte-characters) result)
      result)))

(defun herdr-emacs-trace--snapshot (buffer)
  "Capture the observable editor state relevant to TEXT-mode parity."
  (with-current-buffer buffer
    (let ((result (make-hash-table :test 'equal))
          (restriction-start (point-min))
          (restriction-end (point-max)))
      (puthash "point" (herdr-emacs-trace--position (point) buffer) result)
      (puthash "mark" (herdr-emacs-trace--position (mark t) buffer) result)
      (puthash "mark_active"
               (herdr-emacs-trace--json-false
                (herdr-emacs-trace--mark-active-p))
               result)
      (puthash "region" (herdr-emacs-trace--region buffer) result)
      (puthash "mark_ring" (herdr-emacs-trace--mark-ring buffer) result)
      (puthash "kill_ring" (herdr-emacs-trace--kill-ring) result)
      (puthash "kill_ring_yank_index" (herdr-emacs-trace--kill-ring-index) result)
      (puthash "goal_column" (or goal-column :null) result)
      (puthash "temporary_goal_column" (or temporary-goal-column :null) result)
      (puthash "restriction"
               (let ((restriction (make-hash-table :test 'equal)))
                 (puthash "start"
                          (herdr-emacs-trace--position restriction-start buffer)
                          restriction)
                 (puthash "end"
                          (herdr-emacs-trace--position restriction-end buffer)
                          restriction)
                 (puthash "narrowed"
                          (herdr-emacs-trace--json-false (buffer-narrowed-p))
                          restriction)
                 restriction)
               result)
      (puthash "buffer" (herdr-emacs-trace--buffer-summary buffer) result)
      (puthash "minibuffer" (herdr-emacs-trace--minibuffer) result)
      (puthash "isearch" (herdr-emacs-trace--isearch) result)
      (puthash "message" (or (current-message) :null) result)
      result)))

(defun herdr-emacs-trace--command-name (command)
  (cond
   ((symbolp command) (symbol-name command))
   ((null command) "unknown")
   (t (format "%S" command))))

(defun herdr-emacs-trace--keys ()
  (let ((events (append (this-command-keys-vector) nil)))
    (list
     (key-description (vconcat events))
     (vconcat (mapcar #'single-key-description events)))))

(defun herdr-emacs-trace--pre-command ()
  (when (and herdr-emacs-trace--session
             (not herdr-emacs-trace--in-hook)
             (not (memq this-command
                        '(herdr-emacs-trace-start
                          herdr-emacs-trace-stop))))
    (let ((herdr-emacs-trace--in-hook t)
          (buffer (plist-get herdr-emacs-trace--session :buffer)))
      (when (buffer-live-p buffer)
        (pcase-let ((`(,keys ,events) (herdr-emacs-trace--keys)))
          (let ((command-id (plist-get herdr-emacs-trace--session :next-command-id))
                (parent-id (plist-get (car herdr-emacs-trace--pending) :id)))
            (plist-put herdr-emacs-trace--session :next-command-id (1+ command-id))
            (push (list :id command-id
                        :parent-id parent-id
                        :depth (length herdr-emacs-trace--pending)
                        :command this-command
                        :keys keys
                        :events events
                        :before (herdr-emacs-trace--snapshot buffer))
                  herdr-emacs-trace--pending)))))))

(defun herdr-emacs-trace--post-command ()
  (when (and herdr-emacs-trace--session
             herdr-emacs-trace--pending
             (not herdr-emacs-trace--in-hook))
    (let* ((herdr-emacs-trace--in-hook t)
           (buffer (plist-get herdr-emacs-trace--session :buffer))
           (pending (pop herdr-emacs-trace--pending))
           (steps (plist-get herdr-emacs-trace--session :steps))
           (step (make-hash-table :test 'equal)))
      (when (buffer-live-p buffer)
        (puthash "index" (length steps) step)
        (puthash "command_id" (plist-get pending :id) step)
        (puthash "parent_command_id" (or (plist-get pending :parent-id) :null) step)
        (puthash "depth" (plist-get pending :depth) step)
        (puthash "keys" (plist-get pending :keys) step)
        (puthash "key_events" (plist-get pending :events) step)
        (puthash "command"
                 (herdr-emacs-trace--command-name (plist-get pending :command))
                 step)
        (puthash "before" (plist-get pending :before) step)
        (puthash "after" (herdr-emacs-trace--snapshot buffer) step)
        (plist-put herdr-emacs-trace--session :steps (append steps (list step)))))))

(defun herdr-emacs-trace--install-hooks ()
  ;; Global hooks let us see commands entered in recursive minibuffers and
  ;; isearch while retaining the fixture buffer as the snapshot target.
  (add-hook 'pre-command-hook #'herdr-emacs-trace--pre-command)
  (add-hook 'post-command-hook #'herdr-emacs-trace--post-command))

(defun herdr-emacs-trace--remove-hooks ()
  (remove-hook 'pre-command-hook #'herdr-emacs-trace--pre-command)
  (remove-hook 'post-command-hook #'herdr-emacs-trace--post-command)
  (setq herdr-emacs-trace--pending nil))

;;;###autoload
(defun herdr-emacs-trace-start (output)
  "Start recording this buffer, writing the trace to OUTPUT when stopped."
  (interactive "FWrite Herdr Emacs trace to: ")
  (when herdr-emacs-trace--session
    (user-error "A Herdr Emacs trace is already active"))
  (let ((buffer (current-buffer)))
    (setq herdr-emacs-trace--session
          (list :buffer buffer
                :output (expand-file-name output)
                :text (buffer-substring-no-properties (point-min) (point-max))
                :initial (herdr-emacs-trace--snapshot buffer)
                :next-command-id 0
                :exit-on-stop nil
                :steps nil))
    (herdr-emacs-trace-mode 1)
    (herdr-emacs-trace--install-hooks)
    (message "Recording Emacs behavior; press C-c C-c to save %s" output)))

(defun herdr-emacs-trace--document (session)
  (let* ((buffer (plist-get session :buffer))
         (text (plist-get session :text))
         (result (make-hash-table :test 'equal))
         (reference (make-hash-table :test 'equal))
         (source (make-hash-table :test 'equal)))
    (puthash "schema_version" 1 result)
    (puthash "kind" "herdr-emacs-interactive-trace" result)
    (puthash "implementation" "GNU Emacs" reference)
    (puthash "emacs_version" emacs-version reference)
    (puthash "system_configuration" system-configuration reference)
    (puthash "profile" "interactive recorder; observable state after every command" reference)
    (puthash "reference" reference result)
    (puthash "buffer_name"
             (if (buffer-live-p buffer) (buffer-name buffer) "<killed>")
             source)
    (puthash "text" text source)
    (puthash "sha256" (secure-hash 'sha256 text) source)
    (puthash "source" source result)
    (puthash "initial_state" (plist-get session :initial) result)
    (puthash "steps" (vconcat (plist-get session :steps)) result)
    result))

;;;###autoload
(defun herdr-emacs-trace-stop ()
  "Stop the active recorder and write its JSON document."
  (interactive)
  (unless herdr-emacs-trace--session
    (user-error "No Herdr Emacs trace is active"))
  (let* ((session herdr-emacs-trace--session)
         (buffer (plist-get session :buffer))
         (output (plist-get session :output))
         (document (herdr-emacs-trace--document session)))
    (herdr-emacs-trace--remove-hooks)
    (setq herdr-emacs-trace--session nil)
    (when (buffer-live-p buffer)
      (with-current-buffer buffer
        (setq herdr-emacs-trace-mode nil)
        (force-mode-line-update)))
    (make-directory (file-name-directory output) t)
    (with-temp-file output
      (insert (json-serialize document))
      (insert "\n"))
    (message "Saved %d Emacs command transitions to %s"
             (length (plist-get session :steps)) output)
    (when (plist-get session :exit-on-stop)
      (kill-emacs 0))))

;;;###autoload
(defun herdr-emacs-trace-record-file (input output row col &optional exit-on-stop)
  "Open INPUT as a read-only fixture and record to OUTPUT from ROW and COL."
  (let ((buffer (generate-new-buffer
                 (format "*Herdr trace: %s*" (file-name-nondirectory input)))))
    (switch-to-buffer buffer)
    (fundamental-mode)
    (insert-file-contents input)
    (goto-char (point-min))
    (forward-line row)
    (move-to-column col)
    (setq buffer-read-only t)
    (set-buffer-modified-p nil)
    (transient-mark-mode 1)
    (deactivate-mark)
    (herdr-emacs-trace-start output)
    (plist-put herdr-emacs-trace--session :exit-on-stop exit-on-stop)))

(provide 'emacs_trace)
;;; emacs_trace.el ends here
