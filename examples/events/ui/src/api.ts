// Every call in this SPA goes to the composed component. Nothing here computes a
// number the page then shows: capacity comes from `quota:meter` through
// `GET /api/events/{id}`, the QR is an SVG `qr:encode` rendered, and the refusal to
// admit a ticket twice is `fsm:workflow` reporting the state it is actually in.
export type Json = any;

let token = "";
export const setToken = (t: string) => (token = t);

async function call(path: string, method = "GET", body?: unknown): Promise<[number, Json]> {
  const r = await fetch(path, {
    method,
    headers: {
      "content-type": "application/json",
      ...(token ? { authorization: `Bearer ${token}` } : {}),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  // 204 has no body, and `json()` on an empty one throws.
  const text = await r.text();
  return [r.status, text ? JSON.parse(text) : null];
}

export const api = {
  register: (email: string, password: string) => call("/api/register", "POST", { email, password }),
  login: (email: string, password: string) => call("/api/login", "POST", { email, password }),
  events: (state?: string) => call(`/api/events${state ? `?state=${state}` : ""}`),
  event: (id: string) => call(`/api/events/${id}`),
  createEvent: (b: Json) => call("/api/events", "POST", b),
  // Raw bytes under their own content type — NOT a JSON field. base64 in a document
  // is a third larger than the image and every read of the event pays for it.
  uploadImage: async (id: string, file: File) => {
    const r = await fetch(`/api/events/${id}/image`, {
      method: "POST",
      headers: { "content-type": file.type, ...(token ? { authorization: `Bearer ${token}` } : {}) },
      body: file,
    });
    const t = await r.text();
    return [r.status, t ? JSON.parse(t) : null] as [number, Json];
  },
  claim: (eventId: string) => call(`/api/events/${eventId}/tickets`, "POST", {}),
  myTickets: () => call("/api/tickets"),
  ticket: (id: string) => call(`/api/tickets/${id}`),
  checkin: (code: string) => call("/api/checkin", "POST", { code }),
  offerSwap: (ticketId: string) => call("/api/swaps", "POST", { ticket_id: ticketId }),
  swaps: () => call("/api/swaps"),
  notifications: (after = 0) => call(`/api/notifications?after=${after}`),
  unread: () => call("/api/notifications/unread"),
  markRead: (seqs?: number[]) => call("/api/notifications/read", "POST", seqs ? { seqs } : { through: 0 }),
  prefs: () => call("/api/prefs"),
  putPrefs: (b: Json) => call("/api/prefs", "PUT", b),
  // `EventSource` cannot send an Authorization header, so the stream is opened with
  // a short-lived signed ticket minted by an authenticated POST instead of with the
  // bearer — which in a query string would end up in every access log.
  streamTicket: () => call("/api/notifications/stream-ticket", "POST", {}),
  runReminders: () => call("/api/reminders/run", "POST", {}),
  acceptSwap: (id: string) => call(`/api/swaps/${id}/accept`, "POST", {}),
};
