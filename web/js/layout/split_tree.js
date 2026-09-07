// M26.2 windowing core: a binary split tree of panes (docs/open-questions.md question
// 161, decided by the user: "hand-rolled tiling ... binary split tree, drag handles,
// collapse to a rail, layouts as data"). Framework-free on purpose (no Three.js, no
// DOM) -- exactly like web/js/origin.js's own reasoning (see its module docstring):
// this is the one place the tree arithmetic lives, so a plain `node` process and the
// shipped browser code exercise the exact same bytes, and this module is testable
// headlessly (web/js/layout/layout_tree_check.mjs, driven from
// tests/test_viewer_layout.py) without a DOM.
//
// A pane is either:
//   - a LEAF: { type: 'leaf', id, panelId, collapsed }
//     `panelId` names which content the pane holds (e.g. 'sidebar', 'viewport'); this
//     module knows nothing about what a panelId actually renders -- that is
//     web/js/layout/layout_manager.js's job. `collapsed` is the "collapse to a rail"
//     state (question 161) -- collapsing a leaf never touches its ancestor split's
//     `ratio`, so restoring it returns to exactly the size it had before collapsing.
//   - a SPLIT: { type: 'split', id, direction, ratio, children: [a, b] }
//     `direction` is 'row' (side by side) or 'column' (stacked); `ratio` is the first
//     child's share of the space, in (0, 1) exclusive.
//
// All operations (splitLeaf, closePane, resizeSplit, collapseToRail, restoreFromRail)
// are pure: they return a *new* tree and never mutate their input, so a caller (or a
// test) can always compare the old and new trees by reference or by value.
//
// Serialization is plain JSON (serializeLayout/deserializeLayout) -- "layouts as
// data" (question 161). deserializeLayout validates the whole tree and throws
// LayoutValidationError on anything malformed; it never silently substitutes a
// default. Silently replacing a corrupt saved layout would hide it forever (this
// task's own standing instruction) -- the caller (web/js/layout/persistence.js) is
// the one place that catches this error and decides what the *user* sees.

export class LayoutValidationError extends Error {
  constructor(message) {
    super(message);
    this.name = 'LayoutValidationError';
  }
}

let _counter = 0;
// Exposed only for tests that want a fresh, deterministic id sequence across
// independent operations -- production code almost always passes explicit ids (see
// default_layouts.js) so persisted layouts never depend on process-lifetime counter
// state.
export function resetIdCounterForTests() { _counter = 0; }
function genId(prefix) { return `${prefix}-${_counter++}`; }

const MIN_RATIO = 0.05;
const MAX_RATIO = 0.95;

export function clampRatio(ratio) {
  if (typeof ratio !== 'number' || !Number.isFinite(ratio)) return 0.5;
  return Math.min(MAX_RATIO, Math.max(MIN_RATIO, ratio));
}

// --------------------------------------------------------------------- construction
export function createLeaf(panelId, { id, collapsed = false } = {}) {
  if (typeof panelId !== 'string' || panelId.length === 0) {
    throw new LayoutValidationError('createLeaf: panelId must be a non-empty string');
  }
  return { type: 'leaf', id: id || genId('leaf'), panelId, collapsed: !!collapsed };
}

export function createSplit(direction, ratio, children, { id } = {}) {
  if (direction !== 'row' && direction !== 'column') {
    throw new LayoutValidationError(`createSplit: direction must be 'row' or 'column', got ${JSON.stringify(direction)}`);
  }
  if (!Array.isArray(children) || children.length !== 2) {
    throw new LayoutValidationError('createSplit: children must be an array of exactly 2 nodes');
  }
  return { type: 'split', id: id || genId('split'), direction, ratio: clampRatio(ratio), children };
}

// ------------------------------------------------------------------------ traversal
export function findNode(tree, id) {
  if (!tree) return null;
  if (tree.id === id) return tree;
  if (tree.type === 'split') {
    return findNode(tree.children[0], id) || findNode(tree.children[1], id);
  }
  return null;
}

export function listLeaves(tree) {
  if (!tree) return [];
  if (tree.type === 'leaf') return [tree];
  return [...listLeaves(tree.children[0]), ...listLeaves(tree.children[1])];
}

function collectIds(node, out) {
  out.push(node.id);
  if (node.type === 'split') { collectIds(node.children[0], out); collectIds(node.children[1], out); }
}

// -------------------------------------------------------------------------- split
// Split a leaf into a new split node holding the original leaf plus a fresh leaf for
// `newPanelId`. `position` controls which side the new leaf lands on ('after'
// [default] puts it second, 'before' puts it first) -- this is what lets a caller
// decide "open the new pane to the right" vs. "to the left/above".
export function splitLeaf(tree, leafId, direction, newPanelId, { ratio = 0.5, position = 'after', splitId, newLeafId } = {}) {
  const newLeaf = createLeaf(newPanelId, { id: newLeafId });
  let found = false;
  function rec(node) {
    if (node.id === leafId) {
      if (node.type !== 'leaf') {
        throw new LayoutValidationError(`splitLeaf: node ${leafId} is not a leaf (it is a ${node.type})`);
      }
      found = true;
      const children = position === 'before' ? [newLeaf, node] : [node, newLeaf];
      return createSplit(direction, ratio, children, { id: splitId });
    }
    if (node.type === 'split') {
      return { ...node, children: [rec(node.children[0]), rec(node.children[1])] };
    }
    return node;
  }
  const result = rec(tree);
  if (!found) throw new LayoutValidationError(`splitLeaf: no leaf with id ${leafId} found`);
  return result;
}

// -------------------------------------------------------------------------- close
// Close a pane: its sibling takes the space (question 161). The parent split node is
// discarded entirely (not just emptied) -- the sibling subtree moves up to occupy
// exactly where the split used to be. Closing the tree's only remaining leaf is
// rejected (there would be nothing left to render).
export function closePane(tree, leafId) {
  function rec(node) {
    if (node.id === leafId) return { removed: true };
    if (node.type === 'split') {
      const a = rec(node.children[0]);
      const b = rec(node.children[1]);
      if (a.removed && b.removed) {
        // Unreachable given unique ids, but fail loudly rather than silently if it ever happens.
        throw new LayoutValidationError('closePane: both children resolved to removed -- corrupt tree (duplicate ids?)');
      }
      if (a.removed) return { removed: false, node: b.node };
      if (b.removed) return { removed: false, node: a.node };
      return { removed: false, node: { ...node, children: [a.node, b.node] } };
    }
    return { removed: false, node };
  }
  const result = rec(tree);
  if (result.removed) {
    throw new LayoutValidationError('closePane: cannot close the only remaining pane in the layout');
  }
  return result.node;
}

// -------------------------------------------------------------------------- resize
export function resizeSplit(tree, splitId, ratio) {
  let found = false;
  function rec(node) {
    if (node.id === splitId) {
      if (node.type !== 'split') throw new LayoutValidationError(`resizeSplit: node ${splitId} is not a split (it is a ${node.type})`);
      found = true;
      return { ...node, ratio: clampRatio(ratio) };
    }
    if (node.type === 'split') return { ...node, children: [rec(node.children[0]), rec(node.children[1])] };
    return node;
  }
  const result = rec(tree);
  if (!found) throw new LayoutValidationError(`resizeSplit: no split with id ${splitId} found`);
  return result;
}

// ---------------------------------------------------------- collapse to rail / restore
function setCollapsed(tree, leafId, collapsed) {
  let found = false;
  function rec(node) {
    if (node.id === leafId) {
      if (node.type !== 'leaf') throw new LayoutValidationError(`collapse/restore: node ${leafId} is not a leaf (it is a ${node.type})`);
      found = true;
      return { ...node, collapsed };
    }
    if (node.type === 'split') return { ...node, children: [rec(node.children[0]), rec(node.children[1])] };
    return node;
  }
  const result = rec(tree);
  if (!found) throw new LayoutValidationError(`collapse/restore: no leaf with id ${leafId} found`);
  return result;
}
export function collapseToRail(tree, leafId) { return setCollapsed(tree, leafId, true); }
export function restoreFromRail(tree, leafId) { return setCollapsed(tree, leafId, false); }

// ------------------------------------------------------------ assign panel (question 167)
// Assign a NEW panelId to leaf `leafId` -- the operation behind "an empty pane gets a
// chooser of registered panel types" and "a pane header menu can swap panels" (both
// reduce to the same tree edit: change which panelId one leaf names). A panelId names a
// SINGLE content element (web/js/layout/layout_manager.js's `contentProviders`, one DOM
// node per id, re-parented never cloned -- that module's own docstring); if `newPanelId`
// is already claimed by a DIFFERENT leaf when this runs (moving an existing singleton
// panel, e.g. the console/log panel, from one pane to another), that other leaf reverts
// to a freshly-minted, empty placeholder id FIRST, so the tree never ends up with two
// leaves claiming the same panelId (which would silently race in `LayoutManager.render()`
// -- both would try to `appendChild` the one real element, and only the one rendered last
// in tree order would keep it, leaving the other's pane blank with no chooser and no
// indication why). A brand-new panelId (e.g. a freshly allocated 3D viewport's own id)
// never collides with an existing leaf, so this reduces to a plain rename for that case.
export function assignPanel(tree, leafId, newPanelId) {
  if (typeof newPanelId !== 'string' || newPanelId.length === 0) {
    throw new LayoutValidationError('assignPanel: newPanelId must be a non-empty string');
  }
  let found = false;
  function rec(node) {
    if (node.type === 'split') return { ...node, children: [rec(node.children[0]), rec(node.children[1])] };
    if (node.id === leafId) {
      found = true;
      return { ...node, panelId: newPanelId };
    }
    if (node.panelId === newPanelId) {
      return { ...node, panelId: genId('empty') };
    }
    return node;
  }
  const result = rec(tree);
  if (!found) throw new LayoutValidationError(`assignPanel: no leaf with id ${leafId} found`);
  return result;
}

// --------------------------------------------------------------- serialize / validate
export function validateTree(node, seenIds = new Set(), path = 'root') {
  if (node === null || typeof node !== 'object' || Array.isArray(node)) {
    throw new LayoutValidationError(`${path}: node must be a plain object, got ${JSON.stringify(node)}`);
  }
  if (typeof node.id !== 'string' || node.id.length === 0) {
    throw new LayoutValidationError(`${path}: id must be a non-empty string`);
  }
  if (seenIds.has(node.id)) {
    throw new LayoutValidationError(`${path}: duplicate node id ${JSON.stringify(node.id)}`);
  }
  seenIds.add(node.id);

  if (node.type === 'leaf') {
    if (typeof node.panelId !== 'string' || node.panelId.length === 0) {
      throw new LayoutValidationError(`${path} (leaf ${node.id}): panelId must be a non-empty string`);
    }
    if ('collapsed' in node && typeof node.collapsed !== 'boolean') {
      throw new LayoutValidationError(`${path} (leaf ${node.id}): collapsed must be a boolean`);
    }
    return;
  }
  if (node.type === 'split') {
    if (node.direction !== 'row' && node.direction !== 'column') {
      throw new LayoutValidationError(`${path} (split ${node.id}): direction must be 'row' or 'column', got ${JSON.stringify(node.direction)}`);
    }
    if (typeof node.ratio !== 'number' || !Number.isFinite(node.ratio) || node.ratio <= 0 || node.ratio >= 1) {
      throw new LayoutValidationError(`${path} (split ${node.id}): ratio must be a finite number strictly between 0 and 1, got ${JSON.stringify(node.ratio)}`);
    }
    if (!Array.isArray(node.children) || node.children.length !== 2) {
      throw new LayoutValidationError(`${path} (split ${node.id}): children must be an array of exactly 2 nodes`);
    }
    validateTree(node.children[0], seenIds, `${path}.children[0]`);
    validateTree(node.children[1], seenIds, `${path}.children[1]`);
    return;
  }
  throw new LayoutValidationError(`${path}: unknown node type ${JSON.stringify(node.type)}`);
}

export function serializeLayout(tree) {
  validateTree(tree);
  return JSON.stringify(tree);
}

// Never falls back silently: a malformed string/object throws LayoutValidationError
// (or propagates the JSON.parse SyntaxError, wrapped for a consistent error type) --
// see this module's top docstring and web/js/layout/persistence.js, which is the only
// place that decides what a caller-visible corrupt layout looks like.
export function deserializeLayout(json) {
  let tree;
  if (typeof json === 'string') {
    try {
      tree = JSON.parse(json);
    } catch (e) {
      throw new LayoutValidationError(`invalid layout JSON: ${e.message}`);
    }
  } else {
    tree = json;
  }
  validateTree(tree);
  return tree;
}

export function cloneTree(tree) {
  // Round-trips through the same validated serialize/deserialize path as a real
  // persisted layout would, rather than a separate structuredClone/JSON.parse call --
  // one code path for "make an independent copy of a tree", used by both tests and
  // (later) the layout manager's undo-free "revert to last known good" behaviour.
  return deserializeLayout(serializeLayout(tree));
}
