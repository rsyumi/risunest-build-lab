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
    observations = entries('session-dispatch-observation-complete')
    cleanup = entries('session-dispatch-cleanup-repeated-terminate')
    assert ready['route'] == sent['route'] == 'self-targeted-apple-event'
    assert ready['passed'] and sent['passed'] and sent['status'] == 1
    assert exited['exitCount'] == 1 and exited['runtimeQuitRequests'] == 0
    assert exited['productHandlerReturned'] is True
    assert len(delegates) == len(exited['productDecisions'])
    scenario = phase.removeprefix('session-dispatch-')
    session_requests = [request for request in requests if request['sessionEnd']]
    if scenario == 'initial':
        assert not observations and not cleanup
        assert len(requests) == len(session_requests) == len(delegates) == len(responses) == 1
        assert session_requests[0]['hasDeadline'] is True
        assert responses[0]['exit'] is True and exited['nativeReplies'] == 1
        assert responses[0]['commits'] == responses[0]['maximumActiveCommits'] == responses[0]['checkpoints'] == 1
        assert responses[0]['activeCommits'] == responses[0]['remoteStarts'] == 0
        outcome = 'initial-session-event-delivered'
    else:
        expected = {'local': 'saving', 'drain': 'syncing', 'dialog': 'remote-delayed'}[scenario]
        assert ready['coordinator'] == expected and ready['rendererRequests'] == 1
        assert ready['dialogOpen'] is True
        assert ready['choiceButtons'] == (3 if scenario == 'dialog' else 0)
        assert ready['commits'] == ready['maximumActiveCommits'] == 1
        assert ready['activeCommits'] == (1 if scenario == 'local' else 0)
        assert ready['checkpoints'] == ready['remoteStarts'] == (0 if scenario == 'local' else 1)
        if not session_requests:
            assert len(requests) == len(delegates) == 1
            assert not responses and exited['nativeReplies'] == 0
            if observations:
                observation = one('session-dispatch-observation-complete')
                assert observation['observationMillis'] >= 6_000
                assert observation['rendererRequests'] == 1
                assert observation['sessionRequests'] == observation['rendererReplies'] == 0
                assert observation['coordinator'] == ('remote-delayed' if scenario == 'dialog' else 'syncing')
                assert observation['commits'] == observation['checkpoints'] == observation['remoteStarts'] == 1
                assert observation['maximumActiveCommits'] == 1
                assert observation['activeCommits'] == observation['remoteCancels'] == 0
                assert observation['dialogOpen'] is True
                assert observation['choiceButtons'] == (3 if scenario == 'dialog' else 0)
                assert one('session-dispatch-cleanup-repeated-terminate')['purpose'] == 'cleanup-after-observation'
                order = [next(index for index, entry in enumerate(records) if entry['stage'] == stage)
                         for stage in ['session-dispatch-ready', 'session-dispatch-sent',
                                       'session-dispatch-observation-complete',
                                       'session-dispatch-cleanup-repeated-terminate', 'session-dispatch-exit']]
                assert order == sorted(set(order))
                outcome = 'pending-session-event-not-delivered-within-window'
                assert observation['outcome'] == outcome
            else:
                assert not cleanup
                outcome = 'pending-quit-bypassed-delegate'
        else:
            assert not observations and not cleanup
            assert len(session_requests) == 1 and len(delegates) == 2
            assert session_requests[0]['hasDeadline'] is True
            assert len(responses) == 1 and responses[0]['exit'] is True
            assert exited['nativeReplies'] == 1
            outcome = 'pending-quit-session-upgrade-delivered'
    return {'scenario': scenario, 'outcome': outcome, 'route': 'self-targeted-apple-event',
            'actualOsLogoutExercised': False, 'ready': ready, 'exited': exited,
            'requests': requests, 'responses': responses, 'observations': observations}
