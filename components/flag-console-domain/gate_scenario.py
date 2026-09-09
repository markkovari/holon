def run(api, login):
    admin = login("admin@example.test", "adminpass123", ["admin"])
    eng_billing = login("eng-b@example.test", "engbpass123", ["engineer:billing"])
    eng_search = login("eng-s@example.test", "engspass123", ["engineer:search"])

    code, body = api("POST", "/api/services", {"name": "billing"}, token=eng_billing)
    assert code == 403, ("engineer creates service", code, body)
    code, body = api("POST", "/api/services", {"name": "billing"}, token=admin)
    assert code == 201, ("admin creates service", code, body)

    code, body = api("POST", "/api/services/billing/flags/new-invoice-ui", {"enabled": True}, token=eng_search)
    assert code == 403, ("wrong-service engineer toggles", code, body)

    code, body = api("POST", "/api/services/billing/flags/new-invoice-ui", {"enabled": True}, token=eng_billing)
    assert code == 200, ("owning engineer toggles", code, body)

    code, body = api("POST", "/api/services/billing/flags/new-invoice-ui", {"enabled": False}, token=admin)
    assert code == 200, ("admin overrides any service", code, body)
