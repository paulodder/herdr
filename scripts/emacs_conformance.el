;;; emacs_conformance.el --- GNU Emacs oracle for Herdr TEXT mode -*- lexical-binding: t; -*-

;; Run through scripts/emacs_conformance.py.  The JSON corpus is the shared
;; input language; this file deliberately contains no Herdr-specific expected
;; values.

(require 'json)
(require 'subr-x)

(defun herdr-emacs-conformance--read-json (path)
  (with-temp-buffer
    (insert-file-contents path)
    (json-parse-buffer
     :object-type 'alist
     :array-type 'list
     :null-object nil
     :false-object nil)))

(defun herdr-emacs-conformance--position (position)
  (when position
    (save-excursion
      (goto-char position)
      (let ((result (make-hash-table :test 'equal)))
        (puthash "row" (1- (line-number-at-pos position t)) result)
        (puthash "col" (current-column) result)
        result))))

(defun herdr-emacs-conformance--goto-start (start)
  (goto-char (point-min))
  (forward-line (alist-get 'row start))
  (forward-char (alist-get 'col start)))

(defvar herdr-emacs-conformance--steps nil)
(defvar herdr-emacs-conformance--before nil)
(defvar herdr-emacs-conformance--buffer nil)

(defun herdr-emacs-conformance--snapshot ()
  (with-current-buffer (or herdr-emacs-conformance--buffer (current-buffer))
    (let ((result (make-hash-table :test 'equal)))
      (puthash "point" (herdr-emacs-conformance--position (point)) result)
      (puthash "mark"
               (or (herdr-emacs-conformance--position (mark t)) :null)
               result)
      ;; post-command-hook runs just before the command loop applies
      ;; `deactivate-mark`; report the state visible after the command settles.
      (puthash "mark_active"
               (if (and mark-active (not deactivate-mark)) t :false)
               result)
      (puthash "kill_ring_head"
               (if kill-ring
                   (substring-no-properties (car kill-ring))
                 :null)
               result)
      result)))

(defun herdr-emacs-conformance--pre-command ()
  (setq herdr-emacs-conformance--before
        (herdr-emacs-conformance--snapshot)))

(defun herdr-emacs-conformance--post-command ()
  (let ((keys (key-description (this-command-keys-vector))))
    (when (and herdr-emacs-conformance--before
               this-command
               (not (string-empty-p keys)))
      (let ((step (make-hash-table :test 'equal)))
        (puthash "keys" keys step)
        (puthash "command"
                 (if (symbolp this-command)
                     (symbol-name this-command)
                   (format "%S" this-command))
                 step)
        (puthash "before" herdr-emacs-conformance--before step)
        (puthash "after" (herdr-emacs-conformance--snapshot) step)
        (push step herdr-emacs-conformance--steps)))))

(defun herdr-emacs-conformance--run-case (case)
  (let ((result (make-hash-table :test 'equal))
        (buffer (generate-new-buffer " *herdr-emacs-conformance*")))
    (puthash "name" (alist-get 'name case) result)
    (unwind-protect
        (condition-case error-data
            (save-window-excursion
              ;; `execute-kbd-macro` dispatches in the selected window's
              ;; buffer, so merely using `with-current-buffer` is insufficient.
              (switch-to-buffer buffer)
              (fundamental-mode)
              (transient-mark-mode 1)
              (insert (alist-get 'text case))
              (herdr-emacs-conformance--goto-start (alist-get 'start case))
              (setq buffer-read-only t)
              (let ((kill-ring nil)
                    (kill-ring-yank-pointer nil)
                    (herdr-emacs-conformance--steps nil)
                    (herdr-emacs-conformance--before nil)
                    (herdr-emacs-conformance--buffer buffer)
                    (inhibit-message t)
                    (message-log-max nil))
                (deactivate-mark)
                (add-hook 'pre-command-hook
                          #'herdr-emacs-conformance--pre-command)
                (add-hook 'post-command-hook
                          #'herdr-emacs-conformance--post-command)
                (unwind-protect
                    (execute-kbd-macro (kbd (alist-get 'keys case)))
                  (remove-hook 'pre-command-hook
                               #'herdr-emacs-conformance--pre-command)
                  (remove-hook 'post-command-hook
                               #'herdr-emacs-conformance--post-command))
                (puthash "state" (herdr-emacs-conformance--snapshot) result)
                (puthash "steps"
                         (vconcat (nreverse herdr-emacs-conformance--steps))
                         result)))
          (error
           (puthash "error" (error-message-string error-data) result)))
      (when (buffer-live-p buffer)
        (kill-buffer buffer)))
    result))

(let* ((corpus-path (pop command-line-args-left))
       (corpus (and corpus-path
                    (herdr-emacs-conformance--read-json corpus-path)))
       (result (make-hash-table :test 'equal)))
  (unless corpus-path
    (error "usage: emacs -Q --batch --script scripts/emacs_conformance.el CORPUS"))
  (puthash "emacs_version" emacs-version result)
  (puthash "cases"
           (vconcat
            (mapcar #'herdr-emacs-conformance--run-case
                    (alist-get 'cases corpus)))
           result)
  (princ (json-serialize result)))

;;; emacs_conformance.el ends here
