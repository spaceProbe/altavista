// M26.4 (docs/ui-rework-plan.md): console/log panel -- connection state, server
// messages, and run provenance and hashes. Every field this panel shows already reaches
// the client: `meta.configHash`/`meta.runId`/`meta.bodiesSource`/`meta.published`
// (`altavista/model.py`'s `ScenarioData.to_dict()`, unconditionally present -- `meta`
// always carries at least `published`, and `configHash`/`runId` for a CDM-ingested run,
// confirmed against the real `demo_two_instance.runproducts.bin` fixture, see
// web/js/REPORT_M26_4.md), and connection state / raw server messages come straight
// from web/js/net.js's own `onStatus`/`onMessage` callbacks (already wired in app.js;
// this panel adds a second callback registration, never replaces app.js's own).

/**
 * `sc.meta` (+`sc.imagery.attribution`, +`sc.name`) -> an ordered array of
 * `{label, value}` provenance lines -- only fields that are actually present (never a
 * placeholder "unknown" row for a field a given publish path never set, e.g.
 * `configHash` is absent for a plain `POST /api/scenario` publish, not merely empty).
 * @param {object|null|undefined} sc a wire scenario object
 */
export function provenanceLines(sc) {
  if (!sc) return [];
  const meta = sc.meta || {};
  const lines = [];
  lines.push({ label: 'scenario', value: sc.name });
  if (meta.runId) lines.push({ label: 'run id', value: meta.runId });
  if (meta.configHash) lines.push({ label: 'config hash', value: meta.configHash });
  if (meta.bodiesSource) lines.push({ label: 'bodies source', value: meta.bodiesSource });
  if (meta.published) lines.push({ label: 'published', value: meta.published });
  if (sc.imagery && sc.imagery.attribution) lines.push({ label: 'imagery', value: sc.imagery.attribution });
  return lines;
}

/** One formatted log line for a raw net.js message (`{type, ...}`) -- mirrors
 * web/js/app.js's own `net.onMessage` dispatch (`scenario`/`list`/`clock`/`removed`)
 * without duplicating any of ITS behaviour, only describing what arrived, for a
 * human-readable log. An unrecognized `type` is still shown (its raw type string), never
 * dropped silently -- a console/log panel that hid unknown traffic would defeat its own
 * purpose. */
export function formatServerMessage(msg) {
  const ts = new Date().toISOString().slice(11, 19);
  if (!msg || typeof msg.type !== 'string') return `${ts} · malformed message`;
  switch (msg.type) {
    case 'scenario': return `${ts} · scenario "${msg.scenario && msg.scenario.name}" published`;
    case 'list': return `${ts} · scenario list updated (${(msg.names || []).length})`;
    case 'clock': return `${ts} · clock sync (t=${typeof msg.t === 'number' ? msg.t.toFixed(6) : '?'}${msg.playing ? ', playing' : ''})`;
    case 'removed': return `${ts} · scenario "${msg.name}" removed`;
    default: return `${ts} · ${msg.type}`;
  }
}

const MAX_LOG_LINES = 200;

// -------------------------------------------------------------------------------- DOM
/**
 * Build (once) the panel's DOM: a connection badge, a provenance block, and a scrolling
 * log `<ul>`. Returns handles (`{setConnection, logMessage, setProvenance}`) the caller
 * (web/js/app.js's net.js callbacks) drives over the panel's lifetime -- unlike
 * run_products_panel.js/map_panel.js (whose `render()` is called fresh on every
 * scenario load), the log must ACCUMULATE across calls, so this panel's `render()` is a
 * one-time DOM build, not a full teardown/rebuild per message.
 * @param {HTMLElement} container
 */
export function render(container) {
  container.innerHTML = '';
  const conn = document.createElement('div');
  conn.className = 'av-console-conn badge off';
  conn.textContent = 'offline';
  container.appendChild(conn);

  const prov = document.createElement('dl');
  prov.className = 'av-console-provenance';
  container.appendChild(prov);

  const log = document.createElement('ul');
  log.className = 'av-console-log';
  container.appendChild(log);

  function setConnection(ok) {
    conn.textContent = ok ? 'connected' : 'offline';
    conn.className = 'av-console-conn badge ' + (ok ? 'on' : 'off');
  }

  function setProvenance(sc) {
    prov.innerHTML = '';
    for (const { label, value } of provenanceLines(sc)) {
      const dt = document.createElement('dt'); dt.textContent = label;
      const dd = document.createElement('dd'); dd.textContent = value;
      prov.append(dt, dd);
    }
  }

  function logMessage(msg) {
    const li = document.createElement('li');
    li.textContent = formatServerMessage(msg);
    log.appendChild(li);
    while (log.children.length > MAX_LOG_LINES) log.removeChild(log.firstChild);
    log.scrollTop = log.scrollHeight;
  }

  return { setConnection, setProvenance, logMessage };
}
