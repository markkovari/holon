def run(api, login):
    rec_a = login("rec-a@example.test", "recapass123", ["recruiter"])
    rec_b = login("rec-b@example.test", "recbpass123", ["recruiter"])
    emp = login("emp@example.test", "emppass123", ["employee"])

    code, body = api("POST", "/api/postings", {"title": "Senior Engineer"}, token=emp)
    assert code == 403, ("employee creates posting", code, body)

    code, body = api("POST", "/api/postings", {"title": "Senior Engineer"}, token=rec_a)
    assert code == 201, ("recruiter creates posting", code, body)
    pid = body["id"]

    code, body = api("POST", f"/api/postings/{pid}/referrals", {"candidate_email": "cand@example.test"}, token=emp)
    assert code == 201, ("employee refers a candidate", code, body)

    code, body = api("POST", f"/api/postings/{pid}/close", {}, token=rec_b)
    assert code == 403, ("a DIFFERENT recruiter closes it — always denied, no override", code, body)

    code, body = api("POST", f"/api/postings/{pid}/close", {}, token=rec_a)
    assert code == 200, ("owning recruiter closes it", code, body)
