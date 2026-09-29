#!/usr/bin/env python3
"""Local deterministic ACP peer: no model, credentials, network or shell execution.

Only the peer speaks handwritten JSON-RPC. The production client under test uses
agent-client-protocol. _audit/* methods are test-only ACP extensions.
"""
import json
import os
import sys
import threading
import time

out_lock = threading.Lock()
state_lock = threading.Lock()
sessions, active, loads = {}, {}, {}
sequence = 0
controls = {'failNew': False, 'failLoad': False, 'loadDelay': 0}
cancel_count = 0
wire, replies, rpc_cancels = [], [], []
if os.environ.get('MOCK_PID_FILE'):
    with open(os.environ['MOCK_PID_FILE'], 'w') as f:
        f.write(str(os.getpid()))


def emit_frame(frame, newline=True):
    with out_lock:
        sys.stdout.write(json.dumps(frame) + ('\n' if newline else ''))
        sys.stdout.flush()


def emit(message, newline=True):
    emit_frame({'jsonrpc': '2.0', **message}, newline)


def response(req, result, newline=True):
    emit({'id': req['id'], 'result': result}, newline)


def error(req, message, code=-32000):
    emit({'id': req['id'], 'error': {'code': code, 'message': message}})


def config(sid):
    model = sessions.get(sid, 'history-model')
    return {
        'models': {'currentModelId': model, 'availableModels': [
            {'modelId': 'model-1', 'name': 'One'}, {'modelId': 'model-2', 'name': 'Two'}]},
        'configOptions': [{'id': 'model', 'name': 'Model', 'type': 'select', 'currentValue': model,
                           'options': [{'value': 'model-1', 'name': 'One'}, {'value': 'model-2', 'name': 'Two'}]}],
        'modes': {'currentModeId': 'mode-' + str(model), 'availableModes': []},
    }


def chunk(sid, text):
    emit({'method': 'session/update', 'params': {'sessionId': sid, 'update': {
        'sessionUpdate': 'agent_message_chunk', 'content': {'type': 'text', 'text': text}}}})


def run_prompt(req, stop):
    p = req['params']
    sid, text = p['sessionId'], p['prompt'][0].get('text', '')
    chunk(sid, 'START:' + text)
    cancelled = stop.wait(.7 if text == 'LONG' else .15)
    if cancelled:
        time.sleep(float(controls.get("cancelAckDelay", .08)))  # Delayed cancellation acknowledgement.
    chunk(sid, ('CANCELLED:' if cancelled else 'TAIL:') + text)
    with state_lock:
        active.pop(sid, None)
    result = {} if controls.get('malformedPrompt') else {'stopReason': 'cancelled' if cancelled else 'end_turn'}
    response(req, result, newline=(text != 'SDK_EOF'))
    if text == 'SDK_EOF':
        os._exit(0)


def load(req, delay):
    if delay:
        time.sleep(delay)
    response(req, config(req['params']['sessionId']))


def delayed_echo(req):
    time.sleep(req['params'].get('delay', 0))
    response(req, req['params'].get('value'))


for line in sys.stdin:
    try:
        req = json.loads(line)
    except Exception:
        continue
    if isinstance(req, list):
        replies.extend(req)  # SDK can answer a batch with one response frame.
        continue
    method, p = req.get('method'), req.get('params', {})
    if not method:
        replies.append(req)
        continue
    wire.append(req)
    if method == 'initialize':
        time.sleep(float(os.environ.get('MOCK_INIT_DELAY', '0')))
        response(req, {'protocolVersion': int(os.environ.get('MOCK_PROTOCOL_VERSION', '1')),
                       'agentCapabilities': {'loadSession': True, 'mcpCapabilities': {'http': True}}})
    elif method == 'session/new':
        if controls['failNew']:
            error(req, 'simulated session/new failure')
        elif controls.get('malformedNew'):
            response(req, {'title': 'missing required sessionId'})
        else:
            sequence += 1
            sid = 'session-' + str(sequence)
            sessions[sid] = 'model-' + str(sequence)
            response(req, {'sessionId': sid, **config(sid)})
    elif method in ('session/load', 'session/resume'):
        sid = p['sessionId']
        loads[sid] = loads.get(sid, 0) + 1
        if method == 'session/load' and controls.get('loadUnsupported'):
            error(req, 'load is not supported', -32601)
        elif controls['failLoad']:
            error(req, 'simulated transient session/load failure')
        elif sid in active:
            error(req, 'load overlapped an active prompt')
        else:
            threading.Thread(target=load, args=(req, controls['loadDelay']), daemon=True).start()
    elif method == 'session/list':
        pages = controls.get('listPages', [{'sessions': []}])
        page = 0 if not p.get('cursor') else 1
        response(req, pages[min(page, len(pages) - 1)])
    elif method == 'session/close':
        response(req, {})
    elif method == 'session/prompt':
        sid = p['sessionId']
        with state_lock:
            overlap = sid in active
            if not overlap:
                active[sid] = stop = threading.Event()
        if overlap:
            error(req, 'overlapping prompt for same session')
        else:
            threading.Thread(target=run_prompt, args=(req, stop), daemon=True).start()
    elif method == 'session/cancel':
        cancel_count += 1
        if p['sessionId'] in active:
            active[p['sessionId']].set()
    elif method == '$/cancel_request':
        rpc_cancels.append(p['requestId'])
        # Intentionally still send the delayed result, to exercise late replies.
    elif method == 'session/set_config_option':
        if p['configId'] == 'model':
            sessions[p['sessionId']] = p['value']
        response(req, controls['configAck'] if 'configAck' in controls else config(p['sessionId']))
    elif method == 'session/set_mode':
        response(req, {})
    elif method == '_audit/control':
        controls.update(p)
        response(req, {})
    elif method == '_audit/state':
        with state_lock:
            response(req, {'loads': loads, 'active': list(active), 'cancels': cancel_count,
                           'sessions': sessions, 'pid': os.getpid()})
    elif method == '_audit/records':
        response(req, {'wire': wire, 'replies': replies, 'rpcCancels': rpc_cancels})
    elif method == '_audit/echo':
        threading.Thread(target=delayed_echo, args=(req,), daemon=True).start()
    elif method == '_audit/server_request':
        emit({'id': p['id'], 'method': p['method'], 'params': p['params']})
        response(req, {})
    elif method == '_audit/notify':
        emit({'method': p['method'], 'params': p['params']})
        response(req, {})
    elif method == '_audit/batch':
        emit_frame(p['messages'])
        response(req, {})
    elif method == '_audit/break_transport':
        with out_lock:
            sys.stdout.buffer.write(b'\xff\n' if p.get('kind') == 'utf8' else b'x' * (32 * 1024 * 1024 + 64))
            sys.stdout.buffer.flush()
        break
    elif method == '_audit/exit':
        break  # EOF without replying to a pending request.
    elif method == '_audit/elicitation':
        emit({'id': 'elicit-' + str(req['id']), 'method': 'elicitation/create', 'params': {
            'sessionId': p.get('sessionId', 's1'), 'message': 'Question',
            'requestedSchema': {'type': 'object', 'properties': {'answer': {'type': 'string'}}}}})
        response(req, {})
    elif method == '_audit/permission':
        emit({'id': 'permission-' + str(req['id']), 'method': 'session/request_permission', 'params': {
            'sessionId': p.get('sessionId', 's1'),
            'options': [{'optionId': 'allow', 'name': 'Allow', 'kind': 'allow_once'}],
            'toolCall': {'toolCallId': 't1', 'title': 'Tool'}}})
        response(req, {})
    elif 'id' in req:
        error(req, 'mock does not implement this method', -32601)
