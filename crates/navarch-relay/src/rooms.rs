use super::*;
pub(super) struct Room {
    host: MachineId,
    worker: Option<MachineId>,
    pub(super) expires: Instant,
    step: u8,
    pub(super) closed: bool,
}
impl Registry {
    pub(super) fn expire_rooms(&mut self) {
        let rooms: Vec<_> = self
            .rooms
            .iter()
            .filter(|(_, r)| !r.closed && Instant::now() >= r.expires)
            .map(|(n, _)| n.clone())
            .collect();
        for room in rooms {
            self.close_room(&room);
        }
    }
    pub(super) fn close_for(&mut self, id: &MachineId) {
        let rooms: Vec<_> = self
            .rooms
            .iter()
            .filter(|(_, r)| &r.host == id || r.worker.as_ref() == Some(id))
            .map(|(n, _)| n.clone())
            .collect();
        for room in rooms {
            self.close_room(&room);
        }
    }
    fn close_room(&mut self, name: &str) {
        if let Some(r) = self.rooms.get_mut(name) {
            r.closed = true;
            for id in std::iter::once(&r.host).chain(r.worker.iter()) {
                if let Some(c) = self.connections.get(id) {
                    emit(&c.output, RelayResponse::RoomClosed);
                }
            }
        }
    }
    pub(super) fn room_request(
        &mut self,
        id: &MachineId,
        out: &Output,
        request: RelayRequest,
    ) -> bool {
        let name = match &request {
            RelayRequest::RoomOpen { room }
            | RelayRequest::RoomJoin { room }
            | RelayRequest::RoomData { room, .. }
            | RelayRequest::RoomFail { room } => room.clone(),
            _ => return false,
        };
        if name.len() != 4 || !name.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        if let RelayRequest::RoomOpen { .. } = request {
            if self.rooms.contains_key(&name) {
                return emit(out, RelayResponse::RoomClosed);
            }
            // One concurrent attempt per authenticated identity.
            if self
                .rooms
                .values()
                .any(|r| !r.closed && (&r.host == id || r.worker.as_ref() == Some(id)))
            {
                return false;
            }
            self.rooms.insert(
                name,
                Room {
                    host: id.clone(),
                    worker: None,
                    expires: Instant::now() + Duration::from_secs(600),
                    step: 1,
                    closed: false,
                },
            );
            return emit(out, RelayResponse::RoomOpened);
        }
        let Some(room) = self.rooms.get_mut(&name) else {
            return emit(out, RelayResponse::RoomClosed);
        };
        if room.closed || Instant::now() >= room.expires {
            self.close_room(&name);
            return emit(out, RelayResponse::RoomClosed);
        }
        match request {
            RelayRequest::RoomJoin { .. } => {
                if room.worker.is_some() || &room.host == id {
                    self.close_room(&name);
                    return emit(out, RelayResponse::RoomClosed);
                }
                room.worker = Some(id.clone());
                let Some(host) = self.connections.get(&room.host) else {
                    self.close_room(&name);
                    return false;
                };
                emit(&host.output, RelayResponse::RoomReady { peer: id.clone() })
                    && emit(
                        out,
                        RelayResponse::RoomReady {
                            peer: room.host.clone(),
                        },
                    )
            }
            RelayRequest::RoomData { step, payload, .. } => {
                let worker = room.worker.as_ref();
                let expected = if step % 2 == 1 {
                    Some(&room.host)
                } else {
                    worker
                };
                if expected != Some(id)
                    || step != room.step
                    || worker.is_none()
                    || B64.decode(&payload).map_or(true, |b| b.len() > 4096)
                {
                    self.close_room(&name);
                    return false;
                }
                let to = if step % 2 == 1 {
                    worker
                } else {
                    Some(&room.host)
                };
                let Some(c) = to.and_then(|to| self.connections.get(to)) else {
                    self.close_room(&name);
                    return false;
                };
                room.step += 1;
                if step == 6 {
                    room.closed = true;
                }
                emit(&c.output, RelayResponse::RoomData { step, payload })
            }
            RelayRequest::RoomFail { .. } => {
                if &room.host != id && room.worker.as_ref() != Some(id) {
                    return false;
                }
                self.close_room(&name);
                true
            }
            _ => false,
        }
    }
}
