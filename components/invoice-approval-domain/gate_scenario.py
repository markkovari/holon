def run(api, login):
    clerk = login("clerk@example.test", "clerkpass123", ["clerk"])
    appr_eng = login("appr-eng@example.test", "apprengpass123", ["approver:engineering"])
    appr_sales = login("appr-sales@example.test", "apprsalespass123", ["approver:sales"])

    code, body = api("POST", "/api/invoices",
                      {"vendor": "Acme Corp", "dept": "engineering", "amount": "1099.00", "currency": "USD"},
                      token=clerk)
    assert code == 201, ("clerk creates invoice", code, body)
    iid = body["id"]

    code, body = api("POST", "/api/invoices",
                      {"vendor": "Bad", "dept": "engineering", "amount": "not-a-number", "currency": "USD"},
                      token=clerk)
    assert code == 400, ("bad amount rejected", code, body)

    code, body = api("POST", f"/api/invoices/{iid}/approve", {}, token=appr_sales)
    assert code == 403, ("wrong-department approver", code, body)

    code, body = api("POST", f"/api/invoices/{iid}/approve", {}, token=appr_eng)
    assert code == 200, ("own-department approver", code, body)

    code, body = api("POST", f"/api/invoices/{iid}/approve", {}, token=appr_eng)
    assert code == 409, ("double-approve rejected", code, body)
