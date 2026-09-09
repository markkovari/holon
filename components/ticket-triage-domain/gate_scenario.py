def run(api, login):
    lead = login("lead@example.test", "leadpass123", ["lead"])
    agent_billing = login("agent-b@example.test", "agentbpass123", ["agent:billing"])
    agent_search = login("agent-s@example.test", "agentspass123", ["agent:search"])

    code, body = api("POST", "/api/tickets",
                      {"subject": "double charge", "body": "charged twice for the same order", "queue": "billing"},
                      token=agent_billing)
    assert code == 201, ("create ticket", code, body)
    tid = body["id"]

    code, body = api("GET", "/api/tickets/search?q=charge&queue=billing", token=agent_billing)
    assert code == 200 and any(h.get("id") == tid for h in body.get("results", [])), ("search finds it", code, body)

    code, body = api("POST", f"/api/tickets/{tid}/resolve", {}, token=agent_search)
    assert code == 403, ("wrong-queue agent resolves", code, body)

    code, body = api("POST", f"/api/tickets/{tid}/resolve", {}, token=agent_billing)
    assert code == 200, ("own-queue agent resolves", code, body)

    code, body = api("GET", "/api/tickets/search?q=charge&queue=billing", token=agent_billing)
    assert code == 200 and not any(h.get("id") == tid for h in body.get("results", [])), ("resolved ticket drops out of search", code, body)
