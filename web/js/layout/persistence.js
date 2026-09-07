// M26.2 windowing core: layout persistence (question 161 -- "layouts as data ...
// persisted per viewer in local storage, and shareable as JSON"). Pure functions,
// framework-free like split_tree.js: `storage` is injected (the real
// `window.localStorage` in the browser, a plain in-memory object in tests) so this
// whole module runs headlessly under `node` with no DOM
// (web/js/layout/layout_tree_check.mjs / tests/test_viewer_layout.py).
//
// The one rule this module exists to enforce: a corrupt saved layout is never
// silently replaced by the default. `loadLayout` always returns a usable tree (the
// caller has to render *something*), but when the stored JSON was invalid it also
// returns the error object, untouched, and -- critically -- never calls
// `storage.setItem` itself. The corrupt string stays in storage exactly as it was
// until a caller (web/js/layout/layout_manager.js, wired to a visible "Saved layout
// is corrupt" banner and an explicit "discard and use default" action) decides to
// overwrite it. A silent fallback here would hide a corrupt saved layout forever
// (this task's own standing instruction) -- this is the one seam where that could
// happen, so it is the one seam with a test for it
// (test_persisted_corrupt_layout_is_not_silently_replaced).

import { deserializeLayout, serializeLayout, LayoutValidationError } from './split_tree.js';

export const DEFAULT_STORAGE_KEY = 'altavista.layout.v1';

// Returns { tree, error, source }.
//   - error is null and source is 'persisted' when a valid layout was loaded.
//   - error is null and source is 'default-empty' when nothing was stored yet (not
//     an error -- a fresh viewer has no saved layout).
//   - error is the LayoutValidationError (or a wrapped storage-access error) and
//     source is 'default-after-error' when something was stored but it was not
//     usable. `tree` is still `defaultFactory()` in this case so the caller has
//     something to render, but the caller MUST surface `error` visibly -- see this
//     module's docstring.
export function loadLayout({ storage, key = DEFAULT_STORAGE_KEY, defaultFactory }) {
  let raw;
  try {
    raw = storage.getItem(key);
  } catch (e) {
    return { tree: defaultFactory(), error: e, source: 'default-after-error' };
  }
  if (raw === null || raw === undefined) {
    return { tree: defaultFactory(), error: null, source: 'default-empty' };
  }
  try {
    const tree = deserializeLayout(raw);
    return { tree, error: null, source: 'persisted' };
  } catch (e) {
    return { tree: defaultFactory(), error: e, source: 'default-after-error' };
  }
}

export function saveLayout({ storage, key = DEFAULT_STORAGE_KEY, tree }) {
  storage.setItem(key, serializeLayout(tree));
}

// Export/import as JSON (question 161's "shareable as JSON"): these are thin,
// intentionally -- import goes through the exact same validating deserializeLayout
// as loadLayout, so an imported file gets the exact same "no silent fallback"
// guarantee a corrupt localStorage value gets.
export function exportLayoutJson(tree) {
  return serializeLayout(tree);
}

export function importLayoutJson(json) {
  // Propagates LayoutValidationError on anything malformed -- callers must not catch
  // this and substitute a default without telling the user (see module docstring).
  return deserializeLayout(json);
}

export { LayoutValidationError };
