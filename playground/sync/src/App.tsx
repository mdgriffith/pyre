import { useState, useEffect, useCallback, useRef } from 'react'
import QueryForm from './components/QueryForm'
import MessagePane from './components/MessagePane'
import Clients from './components/Clients'
import { discoverQueries, QueryMetadata } from './queryDiscovery'
import { PyreClient } from '@pyre/client'
import { mountPyreDevtools, type PyreDevtoolsHandle } from '@pyre/client/devtools'
import { schemaMetadata } from '../pyre/generated/typescript/core/schema'
import { userIdForIndex } from './userIds'
import './App.css'

const DATABASE_ID = 'main'

interface Client {
  id: string
  name: string
  pyreClient: PyreClient | null
  connected: boolean
  userId: string | null
  requestedUserId: string | null
  sessionId: string | null
  indexedDbName: string | null
}

interface Event {
  id: string
  type: 'query_sent' | 'query_response' | 'sync_delta'
  timestamp: Date
  data: any
  clientId?: string
}

function App() {
  const [clients, setClients] = useState<Client[]>([
    {
      id: '1',
      name: 'Client 1',
      pyreClient: null,
      sessionId: null,
      connected: false,
      userId: null,
      requestedUserId: userIdForIndex(1),
      indexedDbName: 'pyre-sync-playground-1',
    },
  ])
  const [activeTab, setActiveTab] = useState<'messages' | 'clients'>('clients')
  const clientsRef = useRef<Client[]>([])
  const initialClientConnectedRef = useRef(false)
  const nextUserIndexRef = useRef(2)
  const nextClientNumberRef = useRef<number>(2)
  const pyreClientsRef = useRef<Map<string, PyreClient>>(new Map())
  const connectingClientIdsRef = useRef<Set<string>>(new Set())
  const devtoolsRef = useRef<PyreDevtoolsHandle | null>(null)

  // Keep ref in sync with state
  useEffect(() => {
    clientsRef.current = clients
  }, [clients])
  const [selectedClientId, setSelectedClientId] = useState<string>('1')
  const [events, setEvents] = useState<Event[]>([])
  const [queries, setQueries] = useState<QueryMetadata[]>([])
  const hasConnectedClient = clients.some((client) => client.pyreClient && client.connected)

  // Discover queries on mount
  useEffect(() => {
    const loadQueries = async () => {
      try {
        const discovered = await discoverQueries()
        setQueries(discovered)
      } catch (error) {
        console.error('Failed to discover queries:', error)
      }
    }
    loadQueries()
  }, [])

  const addEvent = useCallback((event: Omit<Event, 'id' | 'timestamp'>) => {
    setEvents((prev) => [
      ...prev,
      {
        ...event,
        id: `${Date.now()}-${Math.random()}`,
        timestamp: new Date(),
      },
    ])
  }, [])

  const connectClient = useCallback(async (clientId: string) => {
    if (connectingClientIdsRef.current.has(clientId) || pyreClientsRef.current.has(clientId)) return
    connectingClientIdsRef.current.add(clientId)

    // Check current state from ref to avoid stale closures
    const client = clientsRef.current.find((c) => c.id === clientId)
    if (!client || client.connected || client.pyreClient) {
      connectingClientIdsRef.current.delete(clientId)
      return
    }

    let userId: string
    if (client.requestedUserId != null) {
      userId = client.requestedUserId
    } else {
      if (clientId === '1') {
        userId = userIdForIndex(1)
        setClients((prev) =>
          prev.map((c) =>
            c.id === clientId ? { ...c, requestedUserId: userId } : c
          )
        )
      } else {
        userId = userIdForIndex(nextUserIndexRef.current)
        nextUserIndexRef.current += 1
        setClients((prev) =>
          prev.map((c) =>
            c.id === clientId ? { ...c, requestedUserId: userId } : c
          )
        )
      }
    }

    let sessionId: string | null = null
    try {
      const loginResponse = await fetch(`http://localhost:3000/login?userId=${encodeURIComponent(String(userId))}`)
      if (!loginResponse.ok) {
        throw new Error(`Login failed: ${loginResponse.status}`)
      }
      const loginData = await loginResponse.json()
      sessionId = loginData.sessionId
    } catch (error: any) {
      console.error('Failed to login:', error)
      addEvent({
        type: 'query_response',
        data: { error: error.message || 'Failed to login' },
        clientId,
      })
      connectingClientIdsRef.current.delete(clientId)
      return
    }

    const queryParams = new URLSearchParams({ sessionId: String(sessionId) }).toString()
    const liveSyncQueryParams = new URLSearchParams({ sessionId: String(sessionId), databaseId: DATABASE_ID }).toString()
    const liveSyncUrl = `http://localhost:3000/sync/events?${liveSyncQueryParams}`
    const baseIndexedDbName = `pyre-sync-playground-${clientId}`

    addEvent({
      type: 'query_sent',
      data: {
        message: 'Connecting to server...',
        url: liveSyncUrl,
        method: 'SSE',
      },
      clientId,
    })

    try {
      const pyreClient = await PyreClient.create({
        schema: schemaMetadata,
        server: {
          baseUrl: 'http://localhost:3000',
          endpoints: {
            catchup: `/sync?${queryParams}`,
            events: `/sync/events?${queryParams}`,
            query: `/db?${queryParams}`,
          },
        },
        cacheNamespace: String(userId),
        indexedDbName: baseIndexedDbName,
      })

      // Store in ref for cleanup
      pyreClientsRef.current.set(clientId, pyreClient)

      // Set up sync state callback
      pyreClient.onSyncState((state) => {
        if (state.status === 'live') {
          addEvent({
            type: 'query_response',
            data: {
              message: 'Sync live',
              tablesSynced: Object.values(state.tables).filter((status) => status === 'live').length,
            },
            clientId,
          })
        } else {
          const table = Object.entries(state.tables).find(([, status]) => status === 'catching_up')?.[0]
          addEvent({
            type: 'query_response',
            data: { message: `Syncing table: ${table ?? '(pending)'}` },
            clientId,
          })
        }
      })

      await pyreClient.setSyncedDatabases([DATABASE_ID])

      setClients((prev) =>
        prev.map((c) =>
          c.id === clientId
            ? {
              ...c,
              pyreClient,
              connected: true,
              userId: userId,
              sessionId,
              indexedDbName: pyreClient.getInternalIndexedDbName(DATABASE_ID),
            }
            : c
        )
      )

      addEvent({
        type: 'query_response',
        data: { message: 'Connected', sessionId },
        clientId,
      })
    } catch (error: any) {
      console.error('Failed to initialize PyreClient:', error)
      addEvent({
        type: 'query_response',
        data: { error: error.message || 'Failed to connect' },
        clientId,
      })
      // Clean up on error
      pyreClientsRef.current.delete(clientId)
    } finally {
      connectingClientIdsRef.current.delete(clientId)
    }
  }, [addEvent])

  useEffect(() => {
    if (hasConnectedClient && !devtoolsRef.current) {
      devtoolsRef.current = mountPyreDevtools()
    }

    return () => {
      if (devtoolsRef.current) {
        devtoolsRef.current.destroy()
        devtoolsRef.current = null
      }
    }
  }, [hasConnectedClient])

  // Connect the initial client once, even in StrictMode.
  useEffect(() => {
    if (!initialClientConnectedRef.current) {
      initialClientConnectedRef.current = true
      connectClient('1')
    }
  }, [connectClient])

  const addNewClient = useCallback(() => {
    // Get the next user ID and increment BEFORE adding to state
    // This ensures we don't have race conditions with React batching
    const newUserId = userIdForIndex(nextUserIndexRef.current)
    nextUserIndexRef.current += 1
    const newId = `${nextClientNumberRef.current}`
    nextClientNumberRef.current += 1

    setClients((prev) => {
      const newClient: Client = {
        id: newId,
        name: `Client ${newId}`,
        pyreClient: null,
        sessionId: null,
        connected: false,
        userId: null,
        requestedUserId: newUserId, // Assign next sequential userId
        indexedDbName: `pyre-sync-playground-${newId}`,
      }
      return [...prev, newClient]
    })
    // Connect after the state commit so connectClient can read the new client from clientsRef.
    setTimeout(() => {
      connectClient(newId)
    }, 0)
  }, [connectClient])

  const submitQuery = useCallback(
    async (queryId: string, params: Record<string, any>) => {
      if (!selectedClientId) {
        throw new Error('No client selected')
      }

      const client = clients.find((c) => c.id === selectedClientId)
      if (!client) {
        throw new Error('Client not found')
      }

      // Include sessionId in query params if client has one
      const sessionId = client.sessionId
      const queryParams = new URLSearchParams({ databaseId: DATABASE_ID })
      if (sessionId) {
        queryParams.set('sessionId', sessionId)
      }
      const url = `http://localhost:3000/db/${queryId}?${queryParams.toString()}`
      const method = 'POST'

      // Log query sent
      addEvent({
        type: 'query_sent',
        data: { queryId, params, url, method, sessionId },
        clientId: selectedClientId,
      })

      try {
        const response = await fetch(url, {
          method,
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(params),
        })

        const result = await response.json()

        addEvent({
          type: 'query_response',
          data: result,
          clientId: selectedClientId,
        })

        // Return result for QueryForm to display
        return result
      } catch (error: any) {
        const errorResult = { error: error.message }
        addEvent({
          type: 'query_response',
          data: errorResult,
          clientId: selectedClientId,
        })
        // Re-throw so QueryForm can catch and display
        throw error
      }
    },
    [selectedClientId, clients, addEvent]
  )

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      // Disconnect all PyreClient instances
      for (const pyreClient of pyreClientsRef.current.values()) {
        pyreClient.disconnect()
      }
      devtoolsRef.current?.destroy()
      devtoolsRef.current = null
      pyreClientsRef.current.clear()
    }
  }, [])

  const resetAllClients = useCallback(async () => {
    if (!confirm('Are you sure you want to delete all clients and clear all IndexedDB data? This cannot be undone.')) {
      return
    }

    // Disconnect all clients first
    for (const pyreClient of pyreClientsRef.current.values()) {
      if (pyreClient) {
        pyreClient.disconnect()
      }
    }

    // Wait for connections to close
    await new Promise(resolve => setTimeout(resolve, 200))

    // Delete all client databases
    const dbNames: string[] = []
    for (const [clientId] of pyreClientsRef.current.entries()) {
      const client = clientsRef.current.find((item) => item.id === clientId)
      if (client?.indexedDbName) {
        dbNames.push(client.indexedDbName)
        console.log(`[Reset] Will delete database for client ${clientId}: ${client.indexedDbName}`)
      }
    }

    // Also collect database names from client IDs (in case clients aren't in ref yet)
    for (let i = 1; i <= 10; i++) {
      const dbName = `pyre-sync-playground-${i}`
      if (!dbNames.includes(dbName)) {
        dbNames.push(dbName)
      }
    }

    try {
      console.log('[Reset] Attempting direct deletion of databases:', dbNames)
      for (const dbName of dbNames) {
        try {
          const deleteRequest = indexedDB.deleteDatabase(dbName)
          await new Promise<void>((resolve) => {
            deleteRequest.onsuccess = () => {
              console.log(`[Reset] Directly deleted database: ${dbName}`)
              resolve()
            }
            deleteRequest.onerror = () => {
              console.warn(`[Reset] Failed to directly delete ${dbName}:`, deleteRequest.error)
              resolve() // Don't fail
            }
            deleteRequest.onblocked = () => {
              console.warn(`[Reset] Database deletion blocked: ${dbName}`)
              resolve() // Don't fail on blocked
            }
          })
        } catch (err) {
          console.warn(`[Reset] Exception deleting ${dbName}:`, err)
        }
      }
      console.log('[Reset] Database deletion complete')
    } catch (error) {
      console.error('[Reset] Error deleting databases:', error)
    }

    // Clear the ref
    pyreClientsRef.current.clear()

    // Reset state to initial state
    setClients([
      {
        id: '1',
        name: 'Client 1',
        pyreClient: null,
        sessionId: null,
        connected: false,
        userId: null,
        requestedUserId: userIdForIndex(1),
        indexedDbName: 'pyre-sync-playground-1',
      },
    ])
    setSelectedClientId('1')
    setEvents([])
    nextUserIndexRef.current = 2
    nextClientNumberRef.current = 2
    connectingClientIdsRef.current.clear()
    initialClientConnectedRef.current = false

    // Wait longer before reconnecting to ensure databases are fully deleted
    setTimeout(() => {
      connectClient('1')
    }, 1000)
  }, [connectClient])

  return (
    <div className="app">
      <header className="app-header">
        <h1>Pyre Sync Playground</h1>
        <button
          onClick={resetAllClients}
          className="reset-button"
          title="Delete all clients and clear IndexedDB"
        >
          Reset All
        </button>
      </header>
      <div className="app-content">
        <div className="left-panel">
          <div className="left-panel-tabs">
            <button
              className={`tab-button ${activeTab === 'clients' ? 'active' : ''}`}
              onClick={() => setActiveTab('clients')}
            >
              Clients
            </button>
            <button
              className={`tab-button ${activeTab === 'messages' ? 'active' : ''}`}
              onClick={() => setActiveTab('messages')}
            >
              Messages
            </button>
          </div>
          <div className="left-panel-content">
            {activeTab === 'messages' ? (
              <MessagePane events={events} clients={clients} />
            ) : (
              <Clients
                clients={clients}
                selectedClientId={selectedClientId}
                onSelectClient={setSelectedClientId}
                onAddClient={addNewClient}
              />
            )}
          </div>
        </div>
        <div className="right-panel">
          <QueryForm
            queries={queries}
            onSubmit={submitQuery}
            selectedClient={clients.find((c) => c.id === selectedClientId)}
          />
        </div>
      </div>
    </div>
  )
}

export default App
