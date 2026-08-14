import type { DevtoolsDatabaseSummary, DevtoolsTableSummary, PyreDevtoolsEvent, PyreDevtoolsInstanceSnapshot, PyreDevtoolsRegistrySnapshot } from './index';
import {
  getPyreDevtoolsInstanceSnapshot,
  getPyreDevtoolsRegistrySnapshot,
  inspectPyreDevtoolsTablePage,
  subscribePyreDevtoolsInstanceEvents,
  subscribePyreDevtoolsRegistry,
} from './devtools-registry';

export interface PyreDevtoolsOptions {
  target?: HTMLElement;
  maxEvents?: number;
}

export interface PyreDevtoolsHandle {
  destroy(): void;
  refresh(): Promise<void>;
}

type Page = 'tables' | 'events' | 'debug';

export function mountPyreDevtools(options: PyreDevtoolsOptions = {}): PyreDevtoolsHandle {
  const target = options.target ?? document.body;
  const element = document.createElement('pyre-devtools') as PyreDevtoolsElement;
  element.configure(options.maxEvents ?? 300);
  target.appendChild(element);

  return {
    destroy() {
      element.remove();
    },
    refresh() {
      return element.refresh();
    },
  };
}

class PyreDevtoolsElement extends HTMLElement {
  private shadowRootRef: ShadowRoot;
  private unsubscribe: (() => void) | null = null;
  private unsubscribeEvents: (() => void) | null = null;
  private registrySnapshot: PyreDevtoolsRegistrySnapshot = { instances: [] };
  private snapshot: PyreDevtoolsInstanceSnapshot | null = null;
  private events: PyreDevtoolsEvent[] = [];
  private maxEvents = 300;
  private open = false;
  private maximized = false;
  private page: Page = 'tables';
  private selectedInstanceId: string | null = null;
  private selectedDatabaseId: string | null = null;
  private selectedTable: string | null = null;
  private selectedEventId: number | null = null;
  private tableInfoOpen = false;
  private tableRows: unknown[] = [];
  private tablePageOffset = 0;
  private tablePageLimit = 100;
  private tableHasMore = false;

  constructor() {
    super();
    this.shadowRootRef = this.attachShadow({ mode: 'open' });
  }

  configure(maxEvents: number): void {
    this.maxEvents = maxEvents;
    this.unsubscribe?.();
    this.unsubscribe = subscribePyreDevtoolsRegistry(() => {
      void this.refresh();
    });
    void this.refresh();
  }

  disconnectedCallback(): void {
    this.unsubscribe?.();
    this.unsubscribeEvents?.();
    this.unsubscribe = null;
    this.unsubscribeEvents = null;
  }

  async refresh(): Promise<void> {
    this.registrySnapshot = await getPyreDevtoolsRegistrySnapshot();
    if (!this.selectedInstanceId || !this.registrySnapshot.instances.some((instance) => instance.instanceId === this.selectedInstanceId)) {
      this.selectedInstanceId = this.registrySnapshot.instances[0]?.instanceId ?? null;
    }
    this.snapshot = this.selectedInstanceId ? await getPyreDevtoolsInstanceSnapshot(this.selectedInstanceId) : null;
    this.events = this.snapshot?.events.slice(0, this.maxEvents) ?? [];
    this.ensureEventSubscription();
    if (!this.selectedDatabaseId || !this.snapshot?.databases.some((database) => database.databaseId === this.selectedDatabaseId)) {
      this.selectedDatabaseId = this.snapshot?.databases[0]?.databaseId ?? null;
      this.selectedTable = null;
    }
    const selectedDatabase = this.getSelectedDatabase();
    if (!this.selectedTable || !selectedDatabase?.tableSummaries?.some((table) => table.name === this.selectedTable)) {
      this.selectedTable = selectedDatabase?.tableSummaries?.[0]?.name ?? null;
      this.tablePageOffset = 0;
    }
    await this.loadSelectedTablePage();
    this.render();
  }

  private ensureEventSubscription(): void {
    this.unsubscribeEvents?.();
    this.unsubscribeEvents = null;
    if (!this.selectedInstanceId) {
      return;
    }
    this.unsubscribeEvents = subscribePyreDevtoolsInstanceEvents(this.selectedInstanceId, (event) => {
      this.events = [event, ...this.events].slice(0, this.maxEvents);
      if (event.operation === 'debug-value.updated' || event.operation.startsWith('sync.') || event.operation.startsWith('database.') || event.operation.startsWith('mutation.')) {
        void this.refresh();
        return;
      }
      this.render({ preserveScroll: true });
    });
  }

  private async loadSelectedTablePage(): Promise<void> {
    if (!this.selectedInstanceId || !this.selectedDatabaseId || !this.selectedTable) {
      this.tableRows = [];
      this.tableHasMore = false;
      return;
    }
    const page = await inspectPyreDevtoolsTablePage({
      instanceId: this.selectedInstanceId,
      databaseId: this.selectedDatabaseId,
      tableName: this.selectedTable,
      offset: this.tablePageOffset,
      limit: this.tablePageLimit,
    });
    this.tableRows = page.rows;
    this.tableHasMore = page.hasMore;
  }

  private render(options: { preserveScroll?: boolean } = {}): void {
    const scrollState = options.preserveScroll ? this.captureScrollState() : [];
    this.shadowRootRef.innerHTML = `${styles}${this.renderBody()}`;
    this.bindEvents();
    this.restoreScrollState(scrollState);
  }

  private renderBody(): string {
    return `
      ${this.open ? '' : '<button class="launcher" title="Open Pyre devtools" aria-label="Open Pyre devtools">P</button>'}
      ${this.open ? this.renderPanel() : ''}
    `;
  }

  private renderPanel(): string {
    return `
      <section class="panel ${this.maximized ? 'maximized' : ''}" aria-label="Pyre devtools">
        <div class="window-actions">
          <button class="window-button maximize" title="${this.maximized ? 'Restore' : 'Maximize'}" aria-label="${this.maximized ? 'Restore Pyre devtools' : 'Maximize Pyre devtools'}">${this.maximized ? lucideIcon('minimize') : lucideIcon('maximize')}</button>
          <button class="window-button panel-close" title="Close" aria-label="Close Pyre devtools">${lucideIcon('close')}</button>
        </div>
        <header class="topbar">
          <div class="brand">Pyre</div>
          ${this.renderTopControls('')}
        </header>
        <aside class="nav">
          ${this.renderNavButton('tables', 'Tables')}
          ${this.renderNavButton('events', `Events ${this.events.length ? `<span>${this.events.length}</span>` : ''}`)}
          ${this.renderNavButton('debug', 'Debug')}
          <button class="refresh">Refresh</button>
        </aside>
        <main class="content">${this.renderPage()}</main>
      </section>
    `;
  }

  private renderNavButton(page: Page, label: string): string {
    return `<button class="nav-item ${this.page === page ? 'active' : ''}" data-page="${page}">${label}</button>`;
  }

  private renderPage(): string {
    if (!this.snapshot) {
      return '<div class="empty">No registered Pyre clients.</div>';
    }
    if (this.page === 'events') {
      return this.renderEventsPage();
    }
    if (this.page === 'debug') {
      return this.renderDebugPage();
    }
    return this.renderTablesPage();
  }

  private renderTopControls(extra: string): string {
    const instances = this.registrySnapshot.instances;
    const databases = this.snapshot?.databases ?? [];
    return `
      <div class="scope-controls">
        <label>Instance
          <select data-instance-select>
            ${instances.length === 0 ? '<option>No instances</option>' : instances.map((instance) => `<option value="${escapeAttr(instance.instanceId)}" ${instance.instanceId === this.selectedInstanceId ? 'selected' : ''}>${escapeHtml(instance.label)}</option>`).join('')}
          </select>
        </label>
        <label>Database
          <select data-database-select>
            ${databases.length === 0 ? '<option>No known databases</option>' : databases.map((database) => `<option value="${escapeAttr(database.databaseId)}" ${database.databaseId === this.selectedDatabaseId ? 'selected' : ''}>${escapeHtml(database.databaseId)}</option>`).join('')}
          </select>
        </label>
      </div>
      ${extra}
    `;
  }

  private getSelectedDatabase(): DevtoolsDatabaseSummary | null {
    return this.snapshot?.databases.find((database) => database.databaseId === this.selectedDatabaseId) ?? null;
  }

  private renderTablesPage(): string {
    const database = this.getSelectedDatabase();
    const tables = database?.tableSummaries ?? [];
    const selected = this.selectedTable ? tables.find((table) => table.name === this.selectedTable) : tables[0];
    return `
      ${database ? this.renderDatabaseSummary(database) : ''}
      <div class="split">
        <section class="table-list">
          <h2>Tables</h2>
          ${tables.map((table) => `
            <button class="table-item ${selected?.name === table.name ? 'active' : ''}" data-table="${escapeAttr(table.name)}">
              <strong>${escapeHtml(table.name)}</strong>
              <span>${table.count ?? 0} rows · ${escapeHtml(table.sync ?? 'unknown')}</span>
            </button>
          `).join('')}
        </section>
        <section class="table-detail">${selected ? this.renderTableDetail(selected) : '<div class="empty">No tables found for this database.</div>'}</section>
      </div>
    `;
  }

  private renderDatabaseSummary(database: DevtoolsDatabaseSummary): string {
    return `
      <div class="database-summary">
        <code>${escapeHtml(database.lifecycle)}</code>
        <span>${database.flaggedForSync ? 'Flagged for sync' : 'Not flagged for sync'}</span>
        <span>${escapeHtml(database.indexedDbName)}</span>
        ${database.error ? `<strong>${escapeHtml(database.error)}</strong>` : ''}
      </div>
    `;
  }

  private renderTableDetail(table: DevtoolsTableSummary): string {
    const schema = this.snapshot?.schema.tables[table.name];
    const schemaColumns = schema?.columns?.map((column) => column.name) ?? [];
    const indices = schema?.indices ?? [];
    const links = schema?.links ?? {};
    const rows = this.tableRows;
    const columns = mergeColumns(schemaColumns, collectColumns(rows));
    const variantGroups = groupByVariant(rows);
    const lastSynced = formatLastSynced(table.cursor?.last_seen_updated_at);
    const approxSize = formatBytes(estimateJsonBytes(rows));
    return `
      <header class="detail-header">
        <div>
          <h1>${escapeHtml(table.name)}</h1>
          <p>${table.count ?? rows.length} records · current page ${approxSize} · last synced ${escapeHtml(lastSynced.relative)}</p>
          ${lastSynced.exact ? `<small title="${escapeAttr(lastSynced.exact)}">${escapeHtml(lastSynced.exact)}</small>` : ''}
        </div>
        <button class="info-toggle" type="button">${this.tableInfoOpen ? 'Hide info' : 'Info'}</button>
      </header>
      ${this.tableInfoOpen ? `
        <div class="info-panel">
          <div class="sync-card">
            <strong>Sync</strong>
            <span>Status: ${escapeHtml(table.sync ?? 'unknown')}</span>
            <span>Last synced: ${escapeHtml(lastSynced.relative)}</span>
            ${lastSynced.exact ? `<span>${escapeHtml(lastSynced.exact)}</span>` : ''}
            <span>Permission hash: ${escapeHtml(table.cursor?.permission_hash ?? 'none')}</span>
          </div>
          <div class="meta-grid">
            <div><strong>Columns</strong>${schema?.columns?.length ? schema.columns.map((column) => `<code>${escapeHtml(column.name)}: ${escapeHtml(column.type)}${column.nullable ? '?' : ''}</code>`).join('') : '<span>Unknown</span>'}</div>
            <div><strong>Union Variants</strong>${variantGroups.length ? variantGroups.map(([name, count]) => `<code>${escapeHtml(name)} ${count}</code>`).join('') : '<span>No _type grouping found</span>'}</div>
            <div><strong>Indices</strong>${indices.length ? indices.map((index) => `<code>${escapeHtml(index.field)}${index.primary ? ' primary' : ''}${index.unique ? ' unique' : ''}</code>`).join('') : '<span>None</span>'}</div>
            <div><strong>Links</strong>${Object.entries(links).length ? Object.entries(links).map(([name, link]) => `<code>${escapeHtml(name)} -> ${escapeHtml(link.to.table)}.${escapeHtml(link.to.column)}</code>`).join('') : '<span>None</span>'}</div>
          </div>
        </div>
      ` : ''}
      <div class="pager"><button data-page-prev ${this.tablePageOffset === 0 ? 'disabled' : ''}>Previous</button><span>Rows ${this.tablePageOffset + 1}-${this.tablePageOffset + rows.length}</span><button data-page-next ${this.tableHasMore ? '' : 'disabled'}>Next</button></div>
      <div class="rows">${this.renderRows(rows, columns)}</div>
    `;
  }

  private renderRows(rows: unknown[], columns: string[]): string {
    if (rows.length === 0) {
      return '<div class="empty">No records in this table.</div>';
    }
    if (columns.length === 0) {
      return rows.map((row) => `<pre>${formatJson(row)}</pre>`).join('');
    }
    return `
      <table>
        <thead><tr>${columns.map((column) => `<th>${escapeHtml(column)}</th>`).join('')}</tr></thead>
        <tbody>
          ${rows.map((row) => {
            const record = row && typeof row === 'object' ? row as Record<string, unknown> : {};
            return `<tr>${columns.map((column) => `<td>${formatCell(record[column])}</td>`).join('')}</tr>`;
          }).join('')}
        </tbody>
      </table>
    `;
  }

  private renderEventsPage(): string {
    const selected = this.events.find((event) => event.id === this.selectedEventId) ?? this.events[0];
    return `
      <div class="split events">
        <section class="event-list">
          <h2>Events</h2>
          ${this.events.length === 0 ? '<div class="empty">No events captured yet.</div>' : this.events.map((event) => `
            <button class="event-item ${selected?.id === event.id ? 'active' : ''}" data-event="${event.id}">
              <strong>${escapeHtml(event.summary)}</strong>
              <span>${new Date(event.timestamp).toLocaleTimeString()}</span>
            </button>
          `).join('')}
        </section>
        <section class="event-detail">${selected ? `<h1>${escapeHtml(selected.summary)}</h1><pre>${formatJson(selected.payload)}</pre>` : ''}</section>
      </div>
    `;
  }

  private renderDebugPage(): string {
    return `
      <h1>Debug</h1>
      <div class="debug-grid">
        <section><h2>Runtime</h2><pre>${formatJson({
          instanceId: this.snapshot?.instanceId,
          cacheNamespace: this.snapshot?.cacheNamespace,
          selectedDatabaseId: this.selectedDatabaseId,
          indexedDbName: this.getSelectedDatabase()?.indexedDbName,
          server: this.snapshot?.server,
          syncProgress: this.snapshot?.syncProgress,
        })}</pre></section>
        <section><h2>Custom Debug Values</h2><pre>${formatJson(this.snapshot?.debugValues)}</pre></section>
        <section><h2>Aggregate Sync State</h2><pre>${formatJson(this.snapshot?.aggregateSyncState)}</pre></section>
        <section><h2>Databases</h2><pre>${formatJson(this.snapshot?.databases)}</pre></section>
        <section><h2>Schema</h2><pre>${formatJson(this.snapshot?.schema)}</pre></section>
      </div>
    `;
  }

  private bindEvents(): void {
    this.shadowRootRef.querySelector('.launcher')?.addEventListener('click', () => {
      this.open = !this.open;
      this.render();
    });
    this.shadowRootRef.querySelector('.refresh')?.addEventListener('click', () => {
      void this.refresh();
    });
    this.shadowRootRef.querySelector('.panel-close')?.addEventListener('click', () => {
      this.open = false;
      this.render();
    });
    this.shadowRootRef.querySelector('.maximize')?.addEventListener('click', () => {
      this.maximized = !this.maximized;
      this.render();
    });
    this.shadowRootRef.querySelector('.info-toggle')?.addEventListener('click', () => {
      this.tableInfoOpen = !this.tableInfoOpen;
      this.render({ preserveScroll: true });
    });
    this.shadowRootRef.querySelectorAll<HTMLElement>('[data-page]').forEach((button) => {
      button.addEventListener('click', () => {
        this.page = button.dataset.page as Page;
        this.render();
      });
    });
    this.shadowRootRef.querySelectorAll<HTMLElement>('[data-table]').forEach((button) => {
      button.addEventListener('click', () => {
        this.selectedTable = button.dataset.table ?? null;
        this.tableInfoOpen = false;
        this.tablePageOffset = 0;
        void this.loadSelectedTablePage().then(() => this.render());
      });
    });
    this.shadowRootRef.querySelector<HTMLSelectElement>('[data-instance-select]')?.addEventListener('change', (event) => {
      this.selectedInstanceId = (event.currentTarget as HTMLSelectElement).value || null;
      this.selectedDatabaseId = null;
      this.selectedTable = null;
      this.tablePageOffset = 0;
      void this.refresh();
    });
    this.shadowRootRef.querySelector<HTMLSelectElement>('[data-database-select]')?.addEventListener('change', (event) => {
      this.selectedDatabaseId = (event.currentTarget as HTMLSelectElement).value || null;
      this.selectedTable = null;
      this.tablePageOffset = 0;
      void this.refresh();
    });
    this.shadowRootRef.querySelector('[data-page-prev]')?.addEventListener('click', () => {
      this.tablePageOffset = Math.max(0, this.tablePageOffset - this.tablePageLimit);
      void this.loadSelectedTablePage().then(() => this.render({ preserveScroll: true }));
    });
    this.shadowRootRef.querySelector('[data-page-next]')?.addEventListener('click', () => {
      this.tablePageOffset += this.tablePageLimit;
      void this.loadSelectedTablePage().then(() => this.render({ preserveScroll: true }));
    });
    this.shadowRootRef.querySelectorAll<HTMLElement>('[data-event]').forEach((button) => {
      button.addEventListener('click', () => {
        this.selectedEventId = Number(button.dataset.event);
        this.render();
      });
    });
    this.shadowRootRef.querySelectorAll<HTMLButtonElement>('[data-copy]').forEach((button) => {
      button.addEventListener('click', () => {
        const text = button.dataset.copy ?? '';
        void copyToClipboard(text).then(() => {
          button.textContent = 'Copied';
          window.setTimeout(() => {
            button.textContent = 'Copy';
          }, 900);
        });
      });
    });
  }

  private captureScrollState(): Array<{ selector: string; scrollTop: number; scrollLeft: number; scrollHeight: number }> {
    return ['.content', '.table-list', '.event-list', '.event-detail'].flatMap((selector) => {
      const element = this.shadowRootRef.querySelector<HTMLElement>(selector);
      if (!element) {
        return [];
      }
      return [{ selector, scrollTop: element.scrollTop, scrollLeft: element.scrollLeft, scrollHeight: element.scrollHeight }];
    });
  }

  private restoreScrollState(scrollState: Array<{ selector: string; scrollTop: number; scrollLeft: number; scrollHeight: number }>): void {
    scrollState.forEach((state) => {
      const element = this.shadowRootRef.querySelector<HTMLElement>(state.selector);
      if (!element) {
        return;
      }
      const insertedAbove = state.selector === '.event-list' && state.scrollTop > 0
        ? Math.max(0, element.scrollHeight - state.scrollHeight)
        : 0;
      element.scrollTop = state.scrollTop + insertedAbove;
      element.scrollLeft = state.scrollLeft;
    });
  }
}

if (!customElements.get('pyre-devtools')) {
  customElements.define('pyre-devtools', PyreDevtoolsElement);
}

function collectColumns(rows: unknown[]): string[] {
  const columns: string[] = [];
  rows.forEach((row) => {
    if (!row || typeof row !== 'object' || Array.isArray(row)) {
      return;
    }
    Object.keys(row).forEach((key) => {
      if (!columns.includes(key)) {
        columns.push(key);
      }
    });
  });
  return columns;
}

function mergeColumns(preferred: string[], discovered: string[]): string[] {
  const columns = [...preferred];
  discovered.forEach((column) => {
    if (!columns.includes(column)) {
      columns.push(column);
    }
  });
  return columns;
}

function groupByVariant(rows: unknown[]): Array<[string, number]> {
  const counts = new Map<string, number>();
  rows.forEach((row) => {
    if (!row || typeof row !== 'object' || Array.isArray(row)) {
      return;
    }
    const variant = (row as Record<string, unknown>)._type;
    if (typeof variant !== 'string' || variant.trim() === '') {
      return;
    }
    counts.set(variant, (counts.get(variant) ?? 0) + 1);
  });
  return Array.from(counts.entries()).sort(([a], [b]) => a.localeCompare(b));
}

function formatCell(value: unknown): string {
  if (value == null) {
    return '<span class="muted">null</span>';
  }
  if (typeof value === 'object') {
    return `<pre>${formatJson(value)}</pre>`;
  }
  if (typeof value === 'string') {
    return `
      <div class="string-cell">
        <span title="${escapeAttr(value)}">${escapeHtml(value)}</span>
        <button class="copy-button" type="button" data-copy="${escapeAttr(value)}" aria-label="Copy cell text">Copy</button>
      </div>
    `;
  }
  return escapeHtml(String(value));
}

async function copyToClipboard(text: string): Promise<void> {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(text);
    return;
  }

  const textarea = document.createElement('textarea');
  textarea.value = text;
  textarea.style.position = 'fixed';
  textarea.style.left = '-9999px';
  document.body.appendChild(textarea);
  textarea.select();
  document.execCommand('copy');
  textarea.remove();
}

function estimateJsonBytes(value: unknown): number {
  const json = JSON.stringify(value) ?? '';
  if (typeof TextEncoder !== 'undefined') {
    return new TextEncoder().encode(json).byteLength;
  }
  return json.length;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const units = ['KB', 'MB', 'GB'];
  let value = bytes / 1024;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value >= 10 ? value.toFixed(0) : value.toFixed(1)} ${units[unitIndex]}`;
}

function formatLastSynced(value: number | null | undefined): { relative: string; exact?: string } {
  if (value == null) {
    return { relative: 'never' };
  }

  const timestamp = value > 1_000_000_000_000 ? value : value * 1000;
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) {
    return { relative: 'unknown' };
  }

  return {
    relative: formatRelativeTime(date),
    exact: date.toLocaleString(),
  };
}

function formatRelativeTime(date: Date): string {
  const diffSeconds = Math.round((date.getTime() - Date.now()) / 1000);
  const absSeconds = Math.abs(diffSeconds);
  const units: Array<[Intl.RelativeTimeFormatUnit, number]> = [
    ['year', 60 * 60 * 24 * 365],
    ['month', 60 * 60 * 24 * 30],
    ['day', 60 * 60 * 24],
    ['hour', 60 * 60],
    ['minute', 60],
    ['second', 1],
  ];
  const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' });
  const [unit, secondsPerUnit] = units.find(([, seconds]) => absSeconds >= seconds) ?? ['second', 1];
  return formatter.format(Math.round(diffSeconds / secondsPerUnit), unit);
}

function formatJson(value: unknown): string {
  return escapeHtml(JSON.stringify(value, null, 2));
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function escapeAttr(value: string): string {
  return escapeHtml(value);
}

function lucideIcon(name: 'maximize' | 'minimize' | 'close'): string {
  const paths = {
    maximize: '<path d="M15 3h6v6"/><path d="m21 3-7 7"/><path d="M9 21H3v-6"/><path d="m3 21 7-7"/>',
    minimize: '<path d="m14 10 7-7"/><path d="M20 10h-6V4"/><path d="m3 21 7-7"/><path d="M4 14h6v6"/>',
    close: '<path d="M18 6 6 18"/><path d="m6 6 12 12"/>',
  };
  return `<svg viewBox="0 0 24 24" aria-hidden="true">${paths[name]}</svg>`;
}

const styles = `
  <style>
    :host { all: initial; color-scheme: light dark; font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
    * { box-sizing: border-box; }
    button { font: inherit; }
    .launcher { position: fixed; right: 18px; bottom: 18px; z-index: 2147483647; width: 42px; height: 42px; border: 0; border-radius: 14px; background: #111827; color: #fff; font-weight: 800; box-shadow: 0 12px 32px rgb(0 0 0 / 28%); cursor: pointer; }
    .panel { position: fixed; right: 18px; bottom: 70px; z-index: 2147483646; width: min(1100px, calc(100vw - 36px)); height: min(720px, calc(100vh - 96px)); display: grid; grid-template-columns: 190px 1fr; grid-template-rows: auto 1fr; overflow: hidden; border: 1px solid rgb(148 163 184 / 35%); border-radius: 18px; background: rgb(248 250 252 / 98%); color: #0f172a; box-shadow: 0 24px 70px rgb(15 23 42 / 30%); }
    .panel.maximized { inset: 12px; width: auto; height: auto; border-radius: 20px; }
    .window-actions { position: absolute; top: 12px; right: 12px; z-index: 2; display: flex; gap: 8px; padding: 5px; border: 1px solid rgb(148 163 184 / 24%); border-radius: 999px; background: rgb(255 255 255 / 72%); backdrop-filter: blur(14px); box-shadow: 0 10px 28px rgb(15 23 42 / 12%); }
    .window-button { display: grid; place-items: center; width: 30px; height: 30px; border: 0; border-radius: 999px; background: transparent; color: #475569; cursor: pointer; }
    .window-button:hover { background: #e2e8f0; color: #0f172a; }
    .window-button svg { width: 17px; height: 17px; fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; }
    .panel-close:hover { background: #fee2e2; color: #b91c1c; }
    .topbar { grid-column: 1 / -1; display: grid; grid-template-columns: 190px 1fr; align-items: center; gap: 16px; padding: 18px 66px 12px 18px; background: #0f172a; color: #e2e8f0; }
    .nav { display: flex; flex-direction: column; gap: 8px; padding: 14px; background: #0f172a; color: #e2e8f0; }
    .brand { padding: 8px 10px; font-size: 28px; font-weight: 850; letter-spacing: -0.04em; }
    .nav-item, .refresh { width: 100%; border: 0; border-radius: 10px; padding: 10px; background: transparent; color: inherit; text-align: left; cursor: pointer; }
    .nav-item span { float: right; opacity: .68; }
    .nav-item.active, .nav-item:hover, .refresh:hover { background: rgb(255 255 255 / 10%); }
    .refresh { margin-top: auto; color: #93c5fd; }
    .content { min-width: 0; overflow: auto; padding: 18px; }
    .scope-controls { display: flex; flex-wrap: wrap; gap: 10px; margin: 0; }
    .scope-controls label { display: grid; gap: 4px; color: #94a3b8; font-size: 12px; font-weight: 700; text-transform: uppercase; letter-spacing: .06em; }
    .scope-controls select { min-width: 190px; border: 1px solid #475569; border-radius: 10px; padding: 8px 10px; background: #1e293b; color: #e2e8f0; font: inherit; text-transform: none; letter-spacing: normal; }
    .database-summary { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-bottom: 14px; padding: 10px 12px; border: 1px solid #e2e8f0; border-radius: 12px; background: #fff; color: #475569; }
    .pager { display: flex; align-items: center; gap: 10px; margin: 0 0 12px; color: #64748b; }
    .pager button { border: 1px solid #cbd5e1; border-radius: 999px; padding: 6px 10px; background: #fff; color: #0f172a; cursor: pointer; }
    .pager button:disabled { opacity: .45; cursor: default; }
    .split { display: grid; grid-template-columns: 270px 1fr; min-height: 100%; gap: 16px; }
    .table-list, .event-list { overflow: auto; border-right: 1px solid #e2e8f0; padding-right: 12px; }
    h1, h2, p { margin: 0; }
    h1 { font-size: 22px; letter-spacing: -0.03em; }
    h2 { margin-bottom: 10px; font-size: 13px; text-transform: uppercase; letter-spacing: .08em; color: #64748b; }
    .table-item, .event-item { width: 100%; display: grid; gap: 3px; margin-bottom: 8px; border: 1px solid #e2e8f0; border-radius: 12px; padding: 10px; background: #fff; color: #0f172a; text-align: left; cursor: pointer; }
    .table-item.active, .event-item.active { border-color: #2563eb; box-shadow: 0 0 0 2px rgb(37 99 235 / 12%); }
    .table-item span, .event-item span, .table-item small, .muted, .note { color: #64748b; }
    .detail-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; margin-bottom: 16px; }
    .detail-header small { display: block; margin-top: 4px; color: #64748b; }
    .info-toggle { border: 1px solid #cbd5e1; border-radius: 999px; padding: 7px 12px; background: #fff; color: #0f172a; cursor: pointer; }
    .info-toggle:hover { border-color: #2563eb; color: #2563eb; }
    .info-panel { display: grid; gap: 12px; margin-bottom: 14px; }
    .sync-card { display: grid; gap: 6px; padding: 12px; border-radius: 12px; background: #f1f5f9; color: #334155; }
    .sync-card strong { color: #0f172a; }
    .meta-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; margin-bottom: 14px; }
    .meta-grid div { display: flex; flex-wrap: wrap; gap: 6px; padding: 10px; border-radius: 12px; background: #f1f5f9; }
    .meta-grid strong { width: 100%; }
    code { padding: 3px 6px; border-radius: 6px; background: #e2e8f0; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; }
    table { width: 100%; border-collapse: collapse; background: #fff; border-radius: 12px; overflow: hidden; }
    th, td { max-width: 320px; border-bottom: 1px solid #e2e8f0; padding: 9px; text-align: left; vertical-align: top; font-size: 12px; }
    th { position: sticky; top: 0; background: #f8fafc; color: #475569; }
    pre { max-width: 100%; overflow: auto; margin: 0; padding: 10px; border-radius: 10px; background: #0f172a; color: #dbeafe; font: 12px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace; }
    td pre { max-height: 160px; padding: 0; background: transparent; color: inherit; }
    .string-cell { display: grid; grid-template-columns: minmax(0, 1fr) auto; align-items: start; gap: 8px; }
    .string-cell span { display: -webkit-box; max-height: 2.9em; overflow: hidden; line-height: 1.45; -webkit-box-orient: vertical; -webkit-line-clamp: 2; overflow-wrap: anywhere; }
    .copy-button { opacity: 0; border: 1px solid #cbd5e1; border-radius: 999px; padding: 2px 7px; background: #fff; color: #475569; font-size: 11px; cursor: pointer; transition: opacity .12s ease, border-color .12s ease, color .12s ease; }
    td:hover .copy-button, .copy-button:focus { opacity: 1; }
    .copy-button:hover { border-color: #2563eb; color: #2563eb; }
    .event-detail pre, .debug-grid pre { max-height: 560px; }
    .debug-grid { display: grid; gap: 14px; }
    .empty { padding: 18px; border: 1px dashed #cbd5e1; border-radius: 12px; color: #64748b; }
    @media (max-width: 760px) { .panel { grid-template-columns: 1fr; left: 12px; right: 12px; bottom: 64px; width: auto; } .panel.maximized { inset: 8px; } .topbar { grid-template-columns: 1fr; padding-right: 96px; } .brand { padding: 0; font-size: 22px; } .nav { flex-direction: row; overflow: auto; } .split { grid-template-columns: 1fr; } .table-list, .event-list { border-right: 0; border-bottom: 1px solid #e2e8f0; max-height: 220px; } }
    @media (prefers-color-scheme: dark) { .panel { background: rgb(15 23 42 / 98%); color: #e2e8f0; border-color: rgb(148 163 184 / 25%); } .window-actions { background: rgb(15 23 42 / 72%); border-color: rgb(148 163 184 / 24%); } .window-button { color: #cbd5e1; } .window-button:hover { background: #334155; color: #fff; } .panel-close:hover { background: rgb(127 29 29 / 70%); color: #fecaca; } .content { background: #111827; } .scope-controls select, .database-summary, .pager button { background: #1e293b; color: #e2e8f0; border-color: #334155; } .table-list, .event-list { border-color: #334155; } .table-item, .event-item, table { background: #1e293b; color: #e2e8f0; border-color: #334155; } th { background: #0f172a; color: #cbd5e1; } td, th { border-color: #334155; } .copy-button { background: #0f172a; color: #cbd5e1; border-color: #475569; } .meta-grid div, .sync-card { background: #1e293b; color: #cbd5e1; } .sync-card strong { color: #e2e8f0; } .info-toggle { background: #1e293b; color: #e2e8f0; border-color: #334155; } code { background: #334155; color: #bfdbfe; } .empty { border-color: #475569; color: #94a3b8; } }
  </style>
`;
