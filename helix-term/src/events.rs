use std::path::Path;

use helix_event::{events, register_event, register_hook};
use helix_view::document::Mode;
use helix_view::events::{
    ConfigDidChange, DiagnosticsDidChange, DocumentDidChange, DocumentDidClose, DocumentDidOpen,
    DocumentFocusLost, DocumentSaved, LanguageServerExited, LanguageServerInitialized,
    LspProgressUpdate, SelectionDidChange,
};
use helix_view::DocumentId;

use crate::commands;
use crate::keymap::MappableCommand;
use crate::term_pty;

events! {
    OnModeSwitch<'a, 'cx> { old_mode: Mode, new_mode: Mode, cx: &'a mut commands::Context<'cx> }
    PostInsertChar<'a, 'cx> { c: char, cx: &'a mut commands::Context<'cx> }
    PostCommand<'a, 'cx> { command: & 'a MappableCommand, cx: &'a mut commands::Context<'cx> }
    TerminalFocusGained<'a, 'cx> { cx: &'a mut commands::Context<'cx> }
    TerminalFocusLost<'a, 'cx> { cx: &'a mut commands::Context<'cx> }
    // Fired synchronously before a `:w`-family command writes a document to disk.
    // A hook can set `*cancel = true` to take over the save entirely (e.g. a
    // plugin-owned buffer that applies its own side effect instead of a literal
    // file write) and skip the built-in write path for this invocation.
    DocumentWillSave<'a, 'cx> {
        doc: DocumentId,
        path: Option<&'a Path>,
        cancel: &'a mut bool,
        cx: &'a mut commands::Context<'cx>
    }
}

pub fn register() {
    register_event::<OnModeSwitch>();
    register_event::<PostInsertChar>();
    register_event::<PostCommand>();
    register_event::<TerminalFocusGained>();
    register_event::<TerminalFocusLost>();
    register_event::<DocumentWillSave>();
    register_event::<DocumentDidOpen>();
    register_event::<DocumentDidChange>();
    register_event::<DocumentDidClose>();
    register_event::<DocumentFocusLost>();
    register_event::<DocumentSaved>();
    register_event::<SelectionDidChange>();
    register_event::<DiagnosticsDidChange>();
    register_event::<LanguageServerInitialized>();
    register_event::<LanguageServerExited>();
    register_event::<LspProgressUpdate>();
    register_event::<ConfigDidChange>();

    register_hook!(move |event: &mut DocumentDidClose<'_>| {
        term_pty::cleanup(event.doc.id());
        Ok(())
    });
}
