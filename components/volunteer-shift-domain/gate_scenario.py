def run(api, login):
    coord = login("coord@example.test", "coordpass123", ["coordinator"])
    vol_a = login("vol-a@example.test", "volapass123", ["volunteer"])
    vol_b = login("vol-b@example.test", "volbpass123", ["volunteer"])

    code, body = api("POST", "/api/shifts", {"title": "Saturday setup", "slots": 1}, token=vol_a)
    assert code == 403, ("volunteer create shift", code, body)

    code, body = api("POST", "/api/shifts", {"title": "Saturday setup", "slots": 1}, token=coord)
    assert code == 201, ("coordinator create shift", code, body)
    shift_id = body["id"]

    code, body = api("POST", f"/api/shifts/{shift_id}/signup", {}, token=vol_a)
    assert code == 201, ("vol_a signup", code, body)
    signup_id = body["signup_id"]

    code, body = api("POST", f"/api/shifts/{shift_id}/signup", {}, token=vol_b)
    assert code == 409, ("vol_b signup on full shift", code, body)

    code, body = api("POST", f"/api/shifts/{shift_id}/signups/{signup_id}/cancel", {}, token=vol_b)
    assert code == 403, ("vol_b cancels vol_a's signup", code, body)

    code, body = api("POST", f"/api/shifts/{shift_id}/signups/{signup_id}/cancel", {}, token=vol_a)
    assert code == 200, ("vol_a cancels own signup", code, body)

    code, body = api("POST", f"/api/shifts/{shift_id}/signup", {}, token=vol_b)
    assert code == 201, ("vol_b signup after cancel freed a slot", code, body)

    for i in range(3):
        code, body = api("POST", "/api/shifts", {"title": f"extra {i}", "slots": 5}, token=coord)
        assert code == 201
        sid = body["id"]
        code, body = api("POST", f"/api/shifts/{sid}/signup", {}, token=vol_a)
        # vol_a already used their weekly quota above once; three more should be allowed
        # (quota is 3/week and this is the 2nd..4th signup for vol_a this week)
    code, body = api("POST", "/api/shifts", {"title": "one too many", "slots": 5}, token=coord)
    sid = body["id"]
    code, body = api("POST", f"/api/shifts/{sid}/signup", {}, token=vol_a)
    assert code == 429, ("vol_a exceeds weekly signup quota", code, body)
