// Copied alongside compiler output by crud_builders.rs, then bundled for Chromium.
import { PyreClient } from '@pyre/client';
import { Note, noteId, database } from './typescript/core/edits';
import { schemaMetadata } from './typescript/core/schema';
import { Elm } from './example.js';

const app = Elm.ComposedExample.init({ flags: null });
const completions: any[] = [];
const counts: any[] = [];
app.ports.completed.subscribe((value: any) => completions.push(value));
app.ports.queryCounts.subscribe((value: any) => counts.push(value));
const rows: Record<string, any[]> = {};
const events: any[] = [];
const client = await PyreClient.create({ schema: schemaMetadata, cacheNamespace: 'generated-example',
  server: { baseUrl: location.origin }, elm: { app } });
for (const instance of ['first', 'second']) {
  await client.run(instance, { operation: 'query', queryShape: { note: { noteKey: true, id: true, title: true } } }, {}, (value: any) => { rows[instance] = value.note; });
  await client.onEntityChanges(instance, { tables: [{ tableName: schemaMetadata.queryFieldToTable.note }] }, event => events.push({ instance, ...event }));
  await client.syncDatabase(instance);
}
Object.assign(window, { example: { rows, events, completions, counts,
  observe() { app.ports.observe.send(null); },
  elmCreate(instance: string) { app.ports.createNote.send(instance); },
  elmUpdate(instance: string) { app.ports.updateNote.send([instance, rows[instance][0].noteKey]); },
  tsUpdate(instance: string) { return client.submit(database('_default', instance), [Note.update(noteId(rows[instance][0].noteKey), { title: 'TS optimistic update' })]); },
  async tsCreate(instance: string) {
    return client.submit(database('_default', instance), [Note.create({ id: 'ordinary', title: 'TS optimistic' })]);
  },
  async rollback(instance: string, id: string) {
    return client.submit(database('_default', instance), [Note.update(noteId(id), { title: 'Must rollback' }), Note.delete(noteId('00000000-0000-7000-8000-000000000999'))]);
  },
} });
