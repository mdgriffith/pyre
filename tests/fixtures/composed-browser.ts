// Copied alongside compiler output by crud_builders.rs, then bundled for Chromium.
import { PyreClient } from '@pyre/client';
import { Document, Note, Reply, documentId, noteId, database } from './typescript/core/edits';
import { schemaMetadata } from './typescript/core/schema';
import { Elm as Ordinary } from './example.js';
import { Elm as Acquisition } from './acquire.js';
import { Elm as Editing } from './editor.js';

const app = Ordinary.ComposedExample.init({ flags: null });
const completions: any[] = [];
const counts: any[] = [];
app.ports.completed.subscribe((value: any) => completions.push(value));
app.ports.queryCounts.subscribe((value: any) => counts.push(value));
const rows: Record<string, any[]> = {};
const replies: Record<string, any[]> = {};
const events: any[] = [];
const client = await PyreClient.create({ schema: schemaMetadata, cacheNamespace: 'generated-example',
  server: { baseUrl: location.origin }, elm: { app } });
for (const instance of ['first', 'second']) {
  await client.run(instance, { operation: 'query', queryShape: { note: { noteKey: true, id: true, title: true } } }, {}, (value: any) => { rows[instance] = value.note; });
  await client.run(instance, { operation: 'query', queryShape: { reply: { replyKey: true, noteKey: true, title: true } } }, {}, (value: any) => { replies[instance] = value.reply; });
  await client.onEntityChanges(instance, { tables: [{ tableName: schemaMetadata.queryFieldToTable.note }] }, event => events.push({ instance, ...event }));
  await client.syncDatabase(instance);
}
const acquired: any[] = [];
const related: any[] = [];
const outgoingAcquisition: any[] = [];
const acquisitionApp = Acquisition.AcquireExample.init({ flags: null });
acquisitionApp.ports.acquired.subscribe((value: any) => acquired.push(value));
acquisitionApp.ports.completed.subscribe((value: any) => related.push(value));
acquisitionApp.ports.pyreStoreOut.subscribe((value: any) => { if (value.type === 'acquire-ids') outgoingAcquisition.push(value); });
const editorApp = Editing.EditorExample.init({ flags: null, node: document.body.appendChild(document.createElement('div')) });
const editorReports: any[] = [];
editorApp.ports.editorReport.subscribe((value: any) => editorReports.push(value));
Object.assign(window, { example: { rows, replies, events, completions, counts, acquired, related, outgoingAcquisition, editorReports,
  observe() { app.ports.observe.send(null); },
  elmCreate(instance: string) { app.ports.createNote.send(instance); },
  elmUpdate(instance: string) { app.ports.updateNote.send([instance, rows[instance][0].noteKey]); },
  tsUpdate(instance: string) { return client.submit(database('_default', instance), [Note.update(noteId(rows[instance][0].noteKey), { title: 'TS optimistic update' })]); },
  async tsCreate(instance: string) {
    const id = Note.newId();
    return { id, result: await client.submit(database('_default', instance), [Note.create(id, { id: 'ordinary', title: 'TS optimistic' })]) };
  },
  async tsRelated(instance: string, title: string) {
    const note = Note.newId();
    const reply = Reply.newId();
    const result = await client.submit(database('_default', instance), [
      Note.create(note, { id: 'related', title }), Reply.create(reply, { noteKey: note, title }),
    ]);
    return { note, reply, result };
  },
  attachAcquisition() { client.attachElmBridge({ app: acquisitionApp }); },
  acquire(explicitReply: boolean) { acquisitionApp.ports.acquire.send(explicitReply); },
  submitRelated(instance: string, title: string) { acquisitionApp.ports.submit.send([instance, title]); },
  attachEditor() { client.attachElmBridge({ app: editorApp }); },
  editorAction(action: string) { editorApp.ports.editorAction.send(action); },
  updateDocument(id: string) { return client.submit(database('_default', 'first'), [Document.update(documentId(id), { title: 'New synced title', published: true })]); },
  async rollback(instance: string, id: string) {
    return client.submit(database('_default', instance), [Note.update(noteId(id), { title: 'Must rollback' }), Note.delete(noteId('00000000-0000-7000-8000-000000000999'))]);
  },
} });
