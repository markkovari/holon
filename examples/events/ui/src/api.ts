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
  claim: (eventId: string) => call(`/api/events/${eventId}/tickets`, "POST", {}),
  myTickets: () => call("/api/tickets"),
  ticket: (id: string) => call(`/api/tickets/${id}`),
  checkin: (code: string) => call("/api/checkin", "POST", { code }),
  offerSwap: (ticketId: string) => call("/api/swaps", "POST", { ticket_id: ticketId }),
  swaps: () => call("/api/swaps"),
  acceptSwap: (id: string) => call(`/api/swaps/${id}/accept`, "POST", {}),
};
