import { useState, useEffect } from 'react'
import { PyreClient } from '@pyre/client'
import { meta as ListUsersAndPosts } from '../../pyre/generated/typescript/core/queries/metadata/listUsersAndPosts'
import './Clients.css'

interface Client {
  id: string
  name: string
  userId: string | null
  requestedUserId: string | null
  connected: boolean
  pyreClient: PyreClient | null
  indexedDbName?: string | null
}

interface ClientsProps {
  clients: Client[]
  selectedClientId: string
  onSelectClient: (clientId: string) => void
  onAddClient: () => void
}

interface ClientData {
  user: any | null
  posts: any[]
}

function ClientCard({
  client,
  isSelected,
  onSelect,
}: {
  client: Client
  isSelected: boolean
  onSelect: () => void
}) {
  const [data, setData] = useState<ClientData>({ user: null, posts: [] })

  useEffect(() => {
    if (!client.pyreClient || !client.connected) {
      setData({ user: null, posts: [] })
      return
    }

    // Query for all users and all posts
    // The query will run immediately (may return empty), then re-run when sync completes
    let subscription: { unsubscribe(): void } | void
    void client.pyreClient.run(
      'main',
      ListUsersAndPosts,
      {}, // No input parameters
      (result: any) => {
        // Find the user for this client
        const userId = client.userId || client.requestedUserId
        const user = result.user?.find((u: any) => u.id === userId) || null

        // Get all posts from the result
        // The query returns both users (with nested posts) and posts (with nested users)
        // We'll use the posts array directly
        const posts = result.post || []

        setData({ user, posts })
      }
    ).then((nextSubscription) => {
      subscription = nextSubscription
    })

    // Return cleanup function that calls unsubscribe if it exists
    return () => {
      if (subscription && typeof subscription === 'object' && 'unsubscribe' in subscription) {
        subscription.unsubscribe()
      }
    }
  }, [client.pyreClient, client.connected, client.userId, client.requestedUserId])

  const getUserName = (): string => {
    if (data.user?.name) {
      return data.user.name
    }
    if (data.user?.email) {
      return data.user.email
    }
    return client.name
  }

  return (
    <div className="clients-client-wrapper">
      <div className="clients-client-name">
        {getUserName()} <span className="clients-client-id">({client.userId ?? client.requestedUserId ?? client.id})</span>
      </div>
      <div
        className={`clients-client-card ${isSelected ? 'selected' : ''}`}
        onClick={onSelect}
      >
        <div className="clients-posts-grid">
          {data.posts.length === 0 ? (
            <div className="clients-empty">No posts</div>
          ) : (
            data.posts.map((post) => (
              <div key={post.id} className={`clients-post-card ${!post.published ? 'unpublished' : ''}`}>
                <div className="clients-post-title">{post.title || 'Untitled'}</div>
                <div className="clients-post-content">{post.content || ''}</div>
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  )
}

export default function Clients({
  clients,
  selectedClientId,
  onSelectClient,
  onAddClient,
}: ClientsProps) {

  return (
    <div className="clients">
      <h2>Clients</h2>
      <div className="clients-grid">
        {clients.map((client) => (
          <ClientCard
            key={client.id}
            client={client}
            isSelected={selectedClientId === client.id}
            onSelect={() => onSelectClient(client.id)}
          />
        ))}
        <div className="clients-client-wrapper clients-add-client-wrapper">
          <div className="clients-add-client-card" onClick={onAddClient}>
            <div className="clients-add-client-text">Add client</div>
          </div>
        </div>
      </div>
    </div>
  )
}
