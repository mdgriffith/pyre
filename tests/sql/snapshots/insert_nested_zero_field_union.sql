-- query: CreateEvent
-- statement 1 of 1 (returns rows)
insert into events (payload, payload__authorParticipantId, payload__voteId, payload__state, updatedAt)
values (case when json_valid($payload) then json_extract($payload, '$._type') else $payload end, case when json_valid($payload) then json_extract($payload, '$.authorParticipantId') else null end, case when json_valid($payload) then json_extract($payload, '$.voteId') else null end, case when json_valid($payload) then json_extract($payload, '$.state._type') else null end, unixepoch()) returning json_object('payload',
  json(case
    when events.payload = 'PlayerVoteHandStateChanged' then
      json_object(
        '_type', 'PlayerVoteHandStateChanged',
        'authorParticipantId', events.payload__authorParticipantId,
        'voteId', events.payload__voteId,
        'state', 
      json(case
        when events.payload__state = 'HandLowered' then json_object('_type', 'HandLowered')
        when events.payload__state = 'HandRaised' then json_object('_type', 'HandRaised')
      end)
      )
  end)) as "event", json_array(json_object('table_name', 'events', 'headers', json_array('id', 'payload', 'payload__authorParticipantId', 'payload__voteId', 'payload__state', 'updatedAt'), 'rows', json_array(json_array("id", "payload", "payload__authorParticipantId", "payload__voteId", "payload__state", "updatedAt")))) as _affectedRows
