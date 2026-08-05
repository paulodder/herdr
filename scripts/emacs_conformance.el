;;; emacs_conformance.el --- GNU Emacs oracle for Herdr TEXT mode -*- lexical-binding: t; -*-

;; Run through scripts/emacs_conformance.py.  The JSON corpus is the shared
;; input language; this file deliberately contains no Herdr-specific expected
;; values.

(require 'json)

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

(defun herdr-emacs-conformance--snapshot ()
  (let ((result (make-hash-table :test 'equal)))
    (puthash "point" (herdr-emacs-conformance--position (point)) result)
    (puthash "mark"
             (or (herdr-emacs-conformance--position (mark t)) :null)
             result)
    (puthash "mark_active" (if mark-active t :false) result)
    (puthash "kill_ring_head"
             (if kill-ring
                 (substring-no-properties (car kill-ring))
               :null)
             result)
    result))

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
                    (inhibit-message t)
                    (message-log-max nil))
                (deactivate-mark)
                (execute-kbd-macro (kbd (alist-get 'keys case)))
                (puthash "state" (herdr-emacs-conformance--snapshot) result)))
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
