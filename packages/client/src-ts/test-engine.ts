// Test transport: acknowledge registration only when Elm requests a stream.
import loadEngine from '../dist/engine.mjs';

export default function loadElm(scope: typeof globalThis) {
  const Elm = loadEngine(scope);
  const initialize = Elm.Main.init;
  Elm.Main.init = (options: any) => {
    const app = initialize(options);
    const connected = () => app.ports.receiveSSEMessage.send({ type: 'connected', connectionId: 'test', databaseId: options.flags.server.databaseId, databaseEpoch: 'test-epoch' });
    app.ports.sseOut.subscribe(connected);
    app.ports.webSocketOut.subscribe(() => app.ports.receiveWebSocketMessage.send({ type: 'connected', connectionId: 'test', databaseId: options.flags.server.databaseId, databaseEpoch: 'test-epoch' }));
    return app;
  };
  return Elm;
}
