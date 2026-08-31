;; A small contract for plugins that own buffers as their UI (a directory
;; listing, a diff view, a picker-as-buffer, ...) instead of hand-rolling a
;; component. Every primitive this builds on - buffer-set-text!,
;; buffer-set-keymap!, the document-* hooks - already works standalone; this
;; just gives plugins a shared way to say "here is a kind of buffer I own,
;; here is what its lifecycle looks like" once, instead of every plugin
;; registering its own global document-* hook and filtering it by hand.
;;
;; This is for *synthetic* buffers a plugin creates and fully owns (via
;; `create-buffer!`), not for tagging regular file buffers by filetype - for
;; that, `(keymap (extension ...))` already does the job.

(require "helix/static.scm")
(require "helix/editor.scm")
(require "helix/keymaps.scm")
(require "helix/commands.scm")

(provide define-buffer-type
         create-buffer!
         buffer-type)

;; type-name (string) -> handlers hash: 'keymap 'on-enter 'on-write 'on-change 'on-close
(define *buffer-type-registry* (hash))
;; doc-id (usize) -> type-name (string), set by create-buffer!
(define *buffer-type-tags* (hash))

(define (%hash-get-or-false hm key)
  (if (hash-contains? hm key) (hash-get hm key) #false))

(define (register-buffer-type! name handlers)
  (set! *buffer-type-registry* (hash-insert *buffer-type-registry* name handlers)))

(define (get-buffer-type-handlers name)
  (%hash-get-or-false *buffer-type-registry* name))

;;@doc
;; Look up the registered buffer type a document was tagged with via
;; `create-buffer!`, or `#false` if it isn't a plugin-owned buffer.
;;
;; ```scheme
;; (buffer-type doc-id) -> (or string? #false)
;; ```
(define (buffer-type doc-id)
  (%hash-get-or-false *buffer-type-tags* (doc-id->usize doc-id)))

(define (%tag-buffer! doc-id type-name)
  (set! *buffer-type-tags*
        (hash-insert *buffer-type-tags* (doc-id->usize doc-id) type-name)))

;;@doc
;; Declare a reusable buffer type: a keymap plus lifecycle callbacks that
;; `create-buffer!` wires up for every buffer created with this type name.
;; `handlers` is a hash literal; every key is optional.
;;
;; ```scheme
;; (define-buffer-type name handlers) -> void?
;;
;; name : string?
;; handlers : hash?
;;   'keymap    : keymap?                       ;; as built by the `keymap` macro
;;   'on-enter  : (-> doc-id? any?)
;;   'on-write  : (-> doc-id? (or string? #false) any?)
;;   'on-change : (-> doc-id? string? any?)
;;   'on-close  : (-> doc-id? any?)
;; ```
;;
;; `on-write` mirrors the `document-will-save` hook: return `#true` to mark
;; the save handled (skipping the built-in write), any other value to let it
;; proceed normally. A handler that takes over the save is responsible for
;; calling `(buffer-mark-saved!)` itself if it wants the modified indicator
;; cleared.
;;
;; # Example
;; ```scheme
;; (define-buffer-type
;;  "oil"
;;  (hash 'keymap (keymap (normal (ret ":oil-open-under-cursor") (q ":oil-close")))
;;        'on-enter (lambda (doc-id) (oil-refresh! doc-id))
;;        'on-write (lambda (doc-id path) (oil-apply-changes! doc-id) #true)))
;;
;; (create-buffer! "oil" (oil-render-listing "."))
;; ```
(define (define-buffer-type name handlers)
  (register-buffer-type! name handlers))

;;@doc
;; Create a new scratch buffer of a type declared with `define-buffer-type`:
;; opens it, applies the type's keymap (if any), sets its initial text (if
;; given), tags it so the shared hooks below route lifecycle events to this
;; type's callbacks, then calls the type's `on-enter` (if any).
;;
;; ```scheme
;; (create-buffer! type-name [initial-text]) -> DocumentId?
;;
;; type-name : string?
;; initial-text : string?
;; ```
(define (create-buffer! type-name . initial-text)
  (define handlers (get-buffer-type-handlers type-name))
  (unless handlers
    (error (string-append "create-buffer!: no buffer type registered under " type-name)))

  (new)
  (define doc-id (editor->doc-id (editor-focus)))

  (%tag-buffer! doc-id type-name)

  (when (hash-contains? handlers 'keymap)
    (buffer-set-keymap! doc-id (hash-get handlers 'keymap)))

  (unless (null? initial-text)
    (buffer-set-text! (car initial-text)))

  (define on-enter (%hash-get-or-false handlers 'on-enter))
  (when on-enter (on-enter doc-id))

  doc-id)

;; Wired once here rather than per-plugin: each dispatches to the owning
;; type's handler (if any) based on the buffer's tag, so individual plugins
;; never register their own global document-* hook and filter it themselves.

(register-hook 'document-will-save
                (lambda (doc-id path)
                  (define type-name (buffer-type doc-id))
                  (define handlers (and type-name (get-buffer-type-handlers type-name)))
                  (define on-write (and handlers (%hash-get-or-false handlers 'on-write)))
                  (if on-write (on-write doc-id path) #false)))

(register-hook 'document-changed
                (lambda (doc-id old-text)
                  (define type-name (buffer-type doc-id))
                  (define handlers (and type-name (get-buffer-type-handlers type-name)))
                  (define on-change (and handlers (%hash-get-or-false handlers 'on-change)))
                  (when on-change (on-change doc-id old-text))))

(register-hook 'document-closed
                (lambda (closed-event)
                  (define doc-id (doc-closed-id closed-event))
                  (define type-name (buffer-type doc-id))
                  (define handlers (and type-name (get-buffer-type-handlers type-name)))
                  (define on-close (and handlers (%hash-get-or-false handlers 'on-close)))
                  (when on-close (on-close doc-id))))
