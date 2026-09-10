// M26.2 windowing core: DOM rendering for the binary split tree (web/js/layout/
// split_tree.js), drag handles with keyboard equivalents, collapse-to-rail UI, the
// export/import-as-JSON toolbar, and the corrupt-layout error banner. This is the one
// module in web/js/layout/ that needs a real `document` -- unlike split_tree.js/
// persistence.js/default_layouts.js, which are framework-free and run headlessly
// under `node` (web/js/layout/layout_tree_check.mjs), this module is exercised by an
// interactive browser check instead (see web/js/layout/REPORT.md).
//
// Existing sidebar/viewport content moves into panes UNCHANGED (this task's brief):
// LayoutManager never clones or rebuilds the #sidebar/#viewport elements -- it only
// `appendChild`s the *existing* DOM nodes (handed in via `contentProviders`) into
// whichever pane wrapper the tree currently says they belong in. web/js/app.js keeps
// calling `document.getElementById(...)` for every control inside them exactly as it
// did before this task; re-parenting an element does not change its id, its children,
// or any event listener already attached to it. See index.html/style.css for the
// other half of this (the pooled elements app.js's ids live on).
import {
  splitLeaf, closePane, resizeSplit, collapseToRail, restoreFromRail, assignPanel,
} from './split_tree.js';
import { loadLayout, saveLayout, exportLayoutJson, importLayoutJson, DEFAULT_STORAGE_KEY } from './persistence.js';
import {
  defaultLayoutForImagery, defaultLayoutForScenario, attachM264Panels,
  ICRF_PANEL_ID, RIC_PANEL_ID, GLOBE_PANEL_ID,
  RUN_PRODUCTS_PANEL_ID, MAP_PANEL_ID, CONSOLE_PANEL_ID, FEASIBILITY_PANEL_ID,
  REGISTERED_PANEL_TYPES, availablePanelChoices,
} from './default_layouts.js';

const RATIO_STEP = 0.02;
const RAIL_PX = 34;

const PANEL_TITLES = {
  sidebar: 'Sidebar',
  viewport: '3D View',
  // M26.3/M26.5 (question 169): the three RPO-default viewport panes used to carry
  // hardcoded frame names ("3D View -- ICRF", "-- Target RIC", "-- Globe") baked into
  // the layout's INTENT, regardless of what frame the scenario actually had to offer --
  // the exact bug the lead found (a pane titled "ICRF" whose own HUD read
  // EarthMJ2000Eq, because the scenario declares no ICRF frame). The base label here is
  // role-neutral and names no frame; `setPaneTitle()`/`web/js/cdm_run.js`'s
  // `viewportPaneTitle()` below append the frame this viewport is ACTUALLY showing,
  // once a scenario has parented its camera in one -- never before, and never a frame
  // this fix cannot back up.
  [ICRF_PANEL_ID]: '3D View',
  [RIC_PANEL_ID]: '3D View',
  [GLOBE_PANEL_ID]: '3D View',
  // M26.4: the three new panels (docs/ui-rework-plan.md).
  [RUN_PRODUCTS_PANEL_ID]: 'Run Products & Scores',
  [MAP_PANEL_ID]: '2D Map',
  [CONSOLE_PANEL_ID]: 'Console / Log',
  // F3b (docs/feasibility-plan.md's F3 milestone): pane-chooser-only (see
  // default_layouts.js's own comment on FEASIBILITY_PANEL_ID for why it is not in
  // attachM264Panels' default tree) -- still needs a title here so a pane the user
  // assigns it to shows a real label, not the raw panelId string.
  [FEASIBILITY_PANEL_ID]: 'Feasibility Study',
};

// M26.5 (question 167): REGISTERED_PANEL_TYPES/availablePanelChoices now live in
// web/js/layout/default_layouts.js (framework-free, headlessly testable under plain
// `node` -- see that module's own comment on why); re-imported above rather than
// redefined here so there is exactly one list.

export class LayoutManager {
  // opts:
  //   root: the DOM element the whole tiled layout renders into
  //   contentProviders: { [panelId]: HTMLElement } -- existing elements to move,
  //     unchanged, into whichever pane currently holds that panelId
  //   errorBanner: an element to show/hide/fill with text on a corrupt persisted
  //     layout (never silently replaced -- see persistence.js's module docstring)
  //   storage: injectable Storage-like object (defaults to window.localStorage)
  //   storageKey: defaults to persistence.js's DEFAULT_STORAGE_KEY
  //   imagery: the active scenario's imagery config (or null before a scenario
  //     loads), used only to pick the *default* layout when nothing is persisted yet
  //   onResize: called after any layout change that can affect pane sizes (the
  //     viewer needs to know its canvas may have moved/resized)
  //   panelFactories: { [basePanelId]: (mintedPanelId) => HTMLElement | null } --
  //     question 167's "new 3D viewport" chooser entry calls
  //     `panelFactories.viewport(mintedId)` to get a real, independent content element
  //     (app.js builds a canvas/labels/hud wrapper and registers a real
  //     `viewer.addViewport(mintedId, ...)` before returning it); a factory returning
  //     null/undefined is a visible error (the error banner), never a silently empty
  //     pane pretending to have chosen something.
  constructor({ root, contentProviders, errorBanner = null, storage = null, storageKey = DEFAULT_STORAGE_KEY, imagery = null, onResize = null, panelFactories = {} }) {
    this.root = root;
    this.contentProviders = contentProviders;
    this.errorBanner = errorBanner;
    this.storage = storage || (typeof window !== 'undefined' ? window.localStorage : null);
    this.storageKey = storageKey;
    this.onResize = onResize;
    this.panelFactories = panelFactories;
    // Question 169: panelId -> the frame-derived title text last set via
    // setPaneTitle(), surviving a render() (PANEL_TITLES alone is the pre-scenario
    // fallback -- see _renderLeaf()); panelId -> the rendered title <span>, so a later
    // setPaneTitle() call can update it directly without a full re-render (this runs
    // from the per-frame HUD update in web/js/app.js, so it must be cheap).
    this._titleOverrides = new Map();
    this._titleEls = new Map();
    this._mintedPanelCount = 0;

    const { tree, error, source } = loadLayout({
      storage: this.storage,
      key: this.storageKey,
      // M26.4: attachM264Panels() adds the run-products/map/console panes to whatever
      // defaultLayoutForImagery() returns -- see default_layouts.js's own module
      // comment on why this wrapping happens here (LayoutManager) rather than inside
      // default_layouts.js's own default-selection functions.
      defaultFactory: () => attachM264Panels(defaultLayoutForImagery(imagery)),
    });
    this.tree = tree;
    // Question 161's "default layout per profile": at construction time (page load,
    // before any scenario has published its `imagery`), the best this module can do
    // is the generic fallback -- see web/js/layout/default_layouts.js's module
    // docstring for why (no profile id reaches the client at all today). Once the
    // first scenario's real `imagery` arrives, applyImageryForDefault() below
    // re-resolves the default *if and only if* the user has not customized or
    // persisted a layout of their own yet (source !== 'persisted'), so a real
    // per-profile default (the day profiles actually diverge) takes effect at the
    // moment it can, without ever clobbering something the user already arranged or
    // explicitly saved.
    this._userHasCustomized = source === 'persisted';
    this._showError(error);
    this.render();
  }

  // Called by the app once a real scenario is known (web/js/app.js's loadScenario()).
  // No-op once the user has interacted with the layout or a real saved layout was
  // loaded -- see the constructor's comment above. M26.3 renamed this from
  // applyImageryForDefault(imagery): the default layout is no longer a function of
  // imagery alone (defaultLayoutForScenario() also checks for a declared RIC frame,
  // web/js/layout/default_layouts.js's own module comment on why) -- app.js's one call
  // site was updated to pass the whole scenario object instead of just `sc.imagery`.
  applyDefaultForScenario(sc) {
    if (this._userHasCustomized) return;
    this.tree = attachM264Panels(defaultLayoutForScenario(sc));
    this.render();
  }

  // ------------------------------------------------------------------------- errors
  _showError(error) {
    if (!this.errorBanner) return;
    if (!error) { this.errorBanner.hidden = true; this.errorBanner.textContent = ''; return; }
    this.errorBanner.hidden = false;
    this.errorBanner.textContent =
      `Saved layout is corrupt and was NOT modified (${this.storageKey}): ${error.message}. ` +
      `Showing a default layout instead -- use "Reset layout" to overwrite the saved one.`;
  }

  // ------------------------------------------------------------------------- persist
  _persist() {
    try {
      saveLayout({ storage: this.storage, key: this.storageKey, tree: this.tree });
    } catch (e) {
      // A tree that fails to serialize/validate here would be this module's own bug
      // (every mutation goes through split_tree.js's validated operations) -- surface
      // it the same visible way rather than losing it, but never as a reason to skip
      // rendering the (already-applied, in-memory) tree change.
      this._showError(e);
    }
  }

  _apply(newTree) {
    this.tree = newTree;
    this._userHasCustomized = true;
    this._showError(null);
    this._persist();
    this.render();
  }

  // M26.3: takes the whole scenario object (was `imagery` alone) -- see
  // applyDefaultForScenario()'s own comment; web/js/layout_bootstrap.js's "Reset
  // layout" button was updated to pass `window.altavistaCurrentScenario` directly.
  resetToDefault(sc) {
    this._userHasCustomized = false;
    this._apply(attachM264Panels(defaultLayoutForScenario(sc)));
    this._userHasCustomized = false; // resetting to default is not "customizing" it
  }

  // -------------------------------------------------------------- export / import
  exportJson() { return exportLayoutJson(this.tree); }

  importJson(jsonText) {
    // Propagates LayoutValidationError to the caller (index.html's import button
    // wires this to a visible alert/message) -- never silently ignored.
    const tree = importLayoutJson(jsonText);
    this._apply(tree);
  }

  // ---------------------------------------------------------------------- rendering
  // render() rebuilds the whole subtree (this.root.innerHTML = ''), so any element
  // that was focused (e.g. a drag handle mid keyboard-resize -- Left/Right/Home/End/
  // Enter all go through _apply() -> render()) is destroyed and replaced by a new
  // node; without restoring focus onto its replacement, the browser silently drops
  // focus to <body> after a single keystroke, which would make repeated keyboard
  // resizing (the normal way to use it) unusable beyond one step. Found live in the
  // M26.2-finish browser check (see web/js/layout/REPORT.md) -- a real ArrowRight on
  // a focused handle updated aria-valuenow correctly but left document.activeElement
  // as <body>, so the very next ArrowRight was silently a no-op.
  render() {
    const refocus = this._captureFocus();
    this._titleEls.clear(); // repopulated by _renderLeaf() below; the old elements are about to be discarded
    this.root.innerHTML = '';
    this.root.appendChild(this._renderNode(this.tree));
    this._restoreFocus(refocus);
    if (this.onResize) this.onResize();
  }

  // ------------------------------------------------------------- pane titles (question 169)
  // Set the title text actually shown for pane `panelId`, overriding PANEL_TITLES'
  // static fallback. Idempotent and cheap to call every render frame (web/js/app.js's
  // per-frame HUD update calls this for every live viewport, primary included) --
  // skips the DOM write entirely when the text has not changed, and never triggers a
  // full render() (a title change alone must not blow away in-progress drag/keyboard
  // focus state, unlike a real tree edit).
  setPaneTitle(panelId, text) {
    if (this._titleOverrides.get(panelId) === text) return;
    this._titleOverrides.set(panelId, text);
    const el = this._titleEls.get(panelId);
    if (el) el.textContent = text;
  }

  _captureFocus() {
    const active = typeof document !== 'undefined' ? document.activeElement : null;
    if (!active || !this.root.contains(active)) return null;
    if (active.classList.contains('av-handle')) {
      return { kind: 'handle', splitId: active.dataset.splitId };
    }
    return null;
  }

  _restoreFocus(captured) {
    if (!captured) return;
    if (captured.kind === 'handle') {
      const el = this.root.querySelector(`.av-handle[data-split-id="${CSS.escape(captured.splitId)}"]`);
      // The split may no longer exist (e.g. a close() elsewhere removed it) -- in
      // that case there is nothing sensible to refocus, so leave focus wherever the
      // browser's default (document.body) puts it rather than guessing.
      if (el && el.tabIndex >= 0) el.focus();
    }
  }

  _renderNode(node) {
    if (node.type === 'leaf') return this._renderLeaf(node);
    return this._renderSplit(node);
  }

  _renderSplit(node) {
    const el = document.createElement('div');
    el.className = `av-split av-split-${node.direction}`;
    el.dataset.splitId = node.id;

    const [a, b] = node.children;
    const aCollapsed = a.type === 'leaf' && a.collapsed;
    const bCollapsed = b.type === 'leaf' && b.collapsed;

    const firstWrap = document.createElement('div');
    firstWrap.className = 'av-split-child';
    firstWrap.appendChild(this._renderNode(a));

    const secondWrap = document.createElement('div');
    secondWrap.className = 'av-split-child';
    secondWrap.appendChild(this._renderNode(b));

    const sizeStyle = (collapsed, ratioShare) => {
      if (collapsed) return `0 0 ${RAIL_PX}px`;
      return `${ratioShare} 1 0`;
    };
    firstWrap.style.flex = sizeStyle(aCollapsed, node.ratio);
    secondWrap.style.flex = sizeStyle(bCollapsed, 1 - node.ratio);

    const handle = this._renderHandle(node, aCollapsed || bCollapsed);

    el.append(firstWrap, handle, secondWrap);
    return el;
  }

  _renderHandle(splitNode, disabled) {
    const handle = document.createElement('div');
    handle.className = `av-handle av-handle-${splitNode.direction}`;
    handle.dataset.splitId = splitNode.id;
    handle.setAttribute('role', 'separator');
    handle.setAttribute('aria-orientation', splitNode.direction === 'row' ? 'vertical' : 'horizontal');
    handle.setAttribute('aria-valuemin', '5');
    handle.setAttribute('aria-valuemax', '95');
    handle.setAttribute('aria-valuenow', String(Math.round(splitNode.ratio * 100)));
    handle.title = 'Drag to resize. Focus and use arrow keys (Home/End for extremes, Enter to reset to 50/50).';
    if (disabled) { handle.classList.add('av-handle-disabled'); return handle; }
    handle.tabIndex = 0;

    let dragStart = null;
    const onPointerMove = (ev) => {
      if (!dragStart) return;
      const rect = this.root.getBoundingClientRect();
      const total = splitNode.direction === 'row' ? rect.width : rect.height;
      if (total <= 0) return;
      const posNow = splitNode.direction === 'row' ? ev.clientX : ev.clientY;
      const delta = (posNow - dragStart.pos) / total;
      this._apply(resizeSplit(this.tree, splitNode.id, dragStart.ratio + delta));
    };
    const onPointerUp = () => {
      dragStart = null;
      window.removeEventListener('pointermove', onPointerMove);
      window.removeEventListener('pointerup', onPointerUp);
    };
    handle.addEventListener('pointerdown', (ev) => {
      ev.preventDefault();
      dragStart = { pos: splitNode.direction === 'row' ? ev.clientX : ev.clientY, ratio: splitNode.ratio };
      window.addEventListener('pointermove', onPointerMove);
      window.addEventListener('pointerup', onPointerUp);
    });

    // Keyboard equivalents (this task's brief: "a handle that only responds to a
    // mouse is incomplete"). Row splits (side-by-side) resize on Left/Right; column
    // splits (stacked) resize on Up/Down -- matching the physical direction of the
    // handle's own drag axis.
    const decKey = splitNode.direction === 'row' ? 'ArrowLeft' : 'ArrowUp';
    const incKey = splitNode.direction === 'row' ? 'ArrowRight' : 'ArrowDown';
    handle.addEventListener('keydown', (ev) => {
      if (ev.key === decKey) { ev.preventDefault(); this._apply(resizeSplit(this.tree, splitNode.id, splitNode.ratio - RATIO_STEP)); }
      else if (ev.key === incKey) { ev.preventDefault(); this._apply(resizeSplit(this.tree, splitNode.id, splitNode.ratio + RATIO_STEP)); }
      else if (ev.key === 'Home') { ev.preventDefault(); this._apply(resizeSplit(this.tree, splitNode.id, 0.05)); }
      else if (ev.key === 'End') { ev.preventDefault(); this._apply(resizeSplit(this.tree, splitNode.id, 0.95)); }
      else if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); this._apply(resizeSplit(this.tree, splitNode.id, 0.5)); }
    });
    return handle;
  }

  _renderLeaf(node) {
    const pane = document.createElement('div');
    pane.className = 'av-pane' + (node.collapsed ? ' av-pane-collapsed' : '');
    pane.dataset.paneId = node.id;

    const header = document.createElement('div');
    header.className = 'av-pane-header';
    const title = document.createElement('span');
    title.className = 'av-pane-title';
    title.textContent = this._titleOverrides.get(node.panelId) || PANEL_TITLES[node.panelId] || node.panelId;
    this._titleEls.set(node.panelId, title);
    header.appendChild(title);

    if (node.collapsed) {
      header.addEventListener('click', () => this._apply(restoreFromRail(this.tree, node.id)));
      header.title = 'Restore pane';
      pane.appendChild(header);
      return pane;
    }

    const hasContent = !!this.contentProviders[node.panelId];
    // Question 167: "a pane header menu can swap a pane's panel" -- offered on every
    // expanded pane, empty or not (an empty pane ALSO gets the bigger chooser buttons
    // in its body below; the header menu is the one control that works uniformly for
    // both, and is the only swap affordance a pane that already holds real content
    // has). A native <select> rather than a custom dropdown: no click-outside/keyboard
    // handling to hand-roll, and it is inherently keyboard-accessible.
    const swap = document.createElement('select');
    swap.className = 'av-pane-swap';
    swap.title = 'Swap this pane’s panel';
    const swapPlaceholder = document.createElement('option');
    swapPlaceholder.value = '';
    swapPlaceholder.textContent = hasContent ? 'Swap panel…' : 'Choose panel…';
    swap.appendChild(swapPlaceholder);
    for (const choice of this._availableChoices(node)) {
      const opt = document.createElement('option');
      opt.value = choice.label;
      opt.textContent = choice.label;
      swap.appendChild(opt);
    }
    swap.value = '';
    swap.addEventListener('change', () => {
      const choice = this._availableChoices(node).find((c) => c.label === swap.value);
      swap.value = '';
      if (choice) this._assignPanelToLeaf(node.id, choice);
    });
    header.appendChild(swap);

    const btns = document.createElement('span');
    btns.className = 'av-pane-btns';
    btns.append(
      this._btn('↕', 'Split below', () => this._apply(splitLeaf(this.tree, node.id, 'column', this._placeholderPanelId()))),
      this._btn('↔', 'Split right', () => this._apply(splitLeaf(this.tree, node.id, 'row', this._placeholderPanelId()))),
      this._btn('–', 'Collapse to rail', () => this._apply(collapseToRail(this.tree, node.id))),
      this._btn('×', 'Close pane', () => { try { this._apply(closePane(this.tree, node.id)); } catch (e) { this._showError(e); } }),
    );
    header.appendChild(btns);
    pane.appendChild(header);

    const body = document.createElement('div');
    body.className = 'av-pane-body';
    const content = this.contentProviders[node.panelId];
    if (content) {
      body.appendChild(content); // re-parent, never clone -- see module docstring
    } else {
      // Question 167: "every empty pane gets a chooser of registered panel types" --
      // replaces the old inert "no panel content registered yet" placeholder text with
      // real buttons, one per currently-available registered type (the header <select>
      // above offers the identical list; these are the more discoverable, one-click
      // affordance the brief calls for specifically for the empty case).
      const chooser = document.createElement('div');
      chooser.className = 'av-pane-chooser';
      const label = document.createElement('div');
      label.className = 'av-pane-chooser-label';
      label.textContent = 'Choose a panel:';
      chooser.appendChild(label);
      const choices = this._availableChoices(node);
      if (choices.length === 0) {
        const none = document.createElement('div');
        none.className = 'av-pane-chooser-empty';
        none.textContent = 'Every registered panel type is already placed elsewhere.';
        chooser.appendChild(none);
      }
      for (const choice of choices) {
        chooser.appendChild(this._btn(choice.label, `Show ${choice.label} in this pane`,
          () => this._assignPanelToLeaf(node.id, choice), 'av-pane-chooser-btn'));
      }
      body.appendChild(chooser);
    }
    pane.appendChild(body);
    return pane;
  }

  // Registered types available to assign to `node` right now: a `factory` type (3D
  // Viewport) is always offered (no uniqueness constraint -- see REGISTERED_PANEL_TYPES'
  // own comment); a singleton type is offered only when no OTHER leaf in the CURRENT
  // tree already claims it (its own element can only be attached to one pane's DOM at
  // a time -- assignPanel() in split_tree.js is the tree-edit half of this rule, this
  // is the "don't even offer the conflict" half).
  _availableChoices(node) {
    return availablePanelChoices(this.tree, node.id);
  }

  // The chooser/header-menu action itself (question 167). A factory choice (3D
  // Viewport) mints a brand-new panelId and asks app.js's registered factory to build
  // a real, independent content element for it -- "its own frame and focus", never a
  // second reference to an existing viewport -- before the tree even changes; a
  // factory that returns nothing is a visible error, not a silently blank pane. A
  // singleton choice just reassigns the existing element via assignPanel(), which
  // itself displaces whatever OTHER pane held it (see that function's own docstring).
  _assignPanelToLeaf(leafId, choice) {
    let targetPanelId = choice.panelId;
    if (choice.factory) {
      targetPanelId = this._mintPanelId(choice.panelId);
      const factory = this.panelFactories[choice.panelId];
      const element = factory ? factory(targetPanelId) : null;
      if (!element) {
        this._showError(new Error(`No panel factory registered for '${choice.panelId}' (question 167: ` +
          'a chosen panel type must always produce real content, never a silently empty pane)'));
        return;
      }
      this.contentProviders[targetPanelId] = element;
    }
    this._apply(assignPanel(this.tree, leafId, targetPanelId));
  }

  _mintPanelId(basePanelId) {
    this._mintedPanelCount += 1;
    return `${basePanelId}-extra-${this._mintedPanelCount}`;
  }

  _placeholderPanelId() {
    // A freshly split leaf has no content assigned yet -- it renders as an empty pane
    // (the chooser above) until the user picks a registered type for it, exactly like
    // any other pane that currently names an unclaimed panelId (assignPanel() in
    // split_tree.js mints the very same shape of id when a singleton panel moves away
    // from a pane).
    this._placeholderCount = (this._placeholderCount || 0) + 1;
    return `placeholder-${this._placeholderCount}`;
  }

  _btn(label, title, onClick, extraClass) {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = extraClass ? `av-pane-btn ${extraClass}` : 'av-pane-btn';
    b.textContent = label;
    b.title = title;
    b.addEventListener('click', (ev) => { ev.stopPropagation(); onClick(); });
    return b;
  }
}
