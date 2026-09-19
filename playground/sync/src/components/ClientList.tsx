import React, { useState } from 'react'
import './ClientList.css'

interface Client {
  id: string
  name: string
  connected: boolean
  sessionId: string | null
  userId: string | null
  requestedUserId: string | null
}

interface ClientListProps {
  clients: Client[]
  selectedClientId: string
  onSelectClient: (clientId: string) => void
  onUpdateUserId: (clientId: string, userId: string | null) => void
}

export default function ClientList({
  clients,
  selectedClientId,
  onSelectClient,
  onUpdateUserId,
}: ClientListProps) {
  const [editingUserId, setEditingUserId] = useState<string | null>(null)
  const [userIdInput, setUserIdInput] = useState<string>('')

  const handleUserIdClick = (e: React.MouseEvent, client: Client) => {
    e.stopPropagation()
    if (!client.connected) {
      setEditingUserId(client.id)
      setUserIdInput(client.requestedUserId?.toString() || '')
    }
  }

  const handleUserIdSubmit = (e: React.FormEvent, clientId: string) => {
    e.preventDefault()
    e.stopPropagation()
    const userId = userIdInput.trim() || null
    if (userId === null || /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(userId)) {
      onUpdateUserId(clientId, userId)
      setEditingUserId(null)
    }
  }

  const handleUserIdKeyDown = (e: React.KeyboardEvent, clientId: string) => {
    if (e.key === 'Enter') {
      handleUserIdSubmit(e, clientId)
    } else if (e.key === 'Escape') {
      setEditingUserId(null)
    }
  }

  return (
    <div className="client-list">
      <h2>Clients</h2>
      <div className="client-list-items">
        {clients.map((client) => (
          <div
            key={client.id}
            className={`client-item ${selectedClientId === client.id ? 'selected' : ''
              }`}
            onClick={() => onSelectClient(client.id)}
          >
            <div className="client-name">
              {client.name}
              {client.connected ? (
                <span className="status connected">✓</span>
              ) : (
                <span className="status disconnected">✗</span>
              )}
            </div>
            <div className="client-info">
              {client.sessionId && (
                <div className="client-session">{client.sessionId}</div>
              )}
              <div className="client-user-id">
                {editingUserId === client.id && !client.connected ? (
                  <form onSubmit={(e) => handleUserIdSubmit(e, client.id)}>
                    <input
                      type="text"
                      value={userIdInput}
                      onChange={(e) => setUserIdInput(e.target.value)}
                      onKeyDown={(e) => handleUserIdKeyDown(e, client.id)}
                      onBlur={() => setEditingUserId(null)}
                      onClick={(e) => e.stopPropagation()}
                      className="user-id-input"
                      placeholder="User ID"
                      autoFocus
                    />
                  </form>
                ) : (
                  <div
                    className={`user-id-display ${!client.connected ? 'editable' : ''}`}
                    onClick={(e) => handleUserIdClick(e, client)}
                    title={!client.connected ? 'Click to edit user ID' : `Connected as user ${client.userId}`}
                  >
                    User ID: {client.requestedUserId ?? client.userId ?? '?'}
                  </div>
                )}
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}
