def validate_dispatch(phase, records):
    def entries(stage):
        return [entry['result'] for entry in records if entry['stage'] == stage]

    def one(stage):
        values = entries(stage)
        if len(values) != 1:
            raise RuntimeError(f'{phase}: exactly one {stage} required')
        return values[0]

    ready = one('session-dispatch-ready')
    sent = one('session-dispatch-sent')
    exited = one('session-dispatch-exit')
    requests = entries('session-dispatch-renderer-request')
    responses = entries('session-dispatch-renderer-response')
    delegates = entries('session-dispatch-delegate')
    assert ready['route'] == sent['route'] == 'self-targeted-apple-event'
    assert ready['passed'] and sent['passed'] and sent['status'] == 1
    assert exited['exitCount'] == 1 and exited['runtimeQuitRequests'] == 0
    assert exited['productHandlerReturned'] is True
    assert len(delegates) == len(exited['productDecisions'])
    scenario = phase.removeprefix('session-dispatch-')
    session_requests = [request for request in requests if request['sessionEnd']]
    if scenario == 'initial':
        assert len(requests) == len(session_requests) == len(delegates) == len(responses) == 1
        assert session_requests[0]['hasDeadline'] is True
        assert responses[0]['exit'] is True and exited['nativeReplies'] == 1
        assert responses[0]['commits'] == responses[0]['maximumActiveCommits'] == responses[0]['checkpoints'] == 1
        assert responses[0]['activeCommits'] == responses[0]['remoteStarts'] == 0
        outcome = 'initial-session-event-delivered'
    else:
        expected = {'local': 'saving', 'drain': 'syncing', 'dialog': 'remote-delayed'}[scenario]
        assert ready['coordinator'] == expected and ready['rendererRequests'] == 1
        assert ready['commits'] == ready['maximumActiveCommits'] == 1
        assert ready['activeCommits'] == (1 if scenario == 'local' else 0)
        assert ready['checkpoints'] == ready['remoteStarts'] == (0 if scenario == 'local' else 1)
        if not session_requests:
            assert len(requests) == len(delegates) == 1
            assert not responses and exited['nativeReplies'] == 0
            outcome = 'pending-quit-bypassed-delegate'
        else:
            assert len(session_requests) == 1 and len(delegates) == 2
            assert session_requests[0]['hasDeadline'] is True
            assert len(responses) == 1 and responses[0]['exit'] is True
            assert exited['nativeReplies'] == 1
            outcome = 'pending-quit-session-upgrade-delivered'
    return {'scenario': scenario, 'outcome': outcome, 'route': 'self-targeted-apple-event',
            'actualOsLogoutExercised': False, 'ready': ready, 'exited': exited,
            'requests': requests, 'responses': responses}
