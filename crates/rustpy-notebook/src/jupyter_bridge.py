"""Explicit Jupyter transport. JSON lines on stdio; kernels and servers owned by this bridge."""
import ast, datetime, json, os, queue, secrets, signal, ssl, subprocess, sys, threading, time, traceback, uuid
from urllib import request as http, parse as urlparse
_OUT=sys.stdout
_LOCK=threading.Lock()
def emit(event, **fields):
    with _LOCK:
        _OUT.write(json.dumps(dict(event=event,**fields),default=str)+'\n');_OUT.flush()
config=json.loads(sys.stdin.readline());cfg=config['connection'];marker=config['marker']
commands=queue.Queue(maxsize=128);closing=threading.Event();km=None;client=None;server=None;ws=None;kernel_id=None;server_session_id=None;session_attempted=False;cookie=None
session=uuid.uuid4().hex;server_session_name='rustpy-'+session;pending={};active=None;execute_done={};idle_parents=set();widgets={};cell_owners={};last_resource=0;base=cfg.get('url','').rstrip('/')+'/'
def log(text): emit('stderr',text=text[:65536]+'\n')
class SafeRedirect(http.HTTPRedirectHandler):
    def redirect_request(self,req,fp,code,msg,headers,newurl):
        target=urlparse.urlsplit(newurl);source=urlparse.urlsplit(req.full_url)
        if target.scheme not in ('http','https'):raise ValueError('Unsupported Jupyter redirect scheme')
        redirected=super().redirect_request(req,fp,code,msg,headers,newurl)
        if redirected and (target.scheme,target.netloc)!=(source.scheme,source.netloc):
            for name in ('Authorization','Cookie','X-XSRFToken'):redirected.remove_header(name)
        return redirected
def api(path,method='GET',data=None,timeout=15):
    headers={'Content-Type':'application/json'}
    if cfg.get('token'):headers['Authorization']='token '+cfg['token']
    if cookie:
        headers['Cookie']=cookie
        for part in cookie.split('; '):
            if part.startswith('_xsrf='):headers['X-XSRFToken']=part.partition('=')[2]
    req=http.Request(base+path,data=None if data is None else json.dumps(data).encode(),headers=headers,method=method)
    context=ssl.create_default_context(cafile=cfg.get('ca_file') or None)
    if not cfg.get('verify_ssl',True):context.check_hostname=False;context.verify_mode=ssl.CERT_NONE
    opener=http.build_opener(http.HTTPSHandler(context=context),SafeRedirect())
    with opener.open(req,timeout=2 if closing.is_set() else timeout) as response:
        raw=response.read(16*1024*1024+1)
        if len(raw)>16*1024*1024:raise RuntimeError('Server reply exceeds 16 MiB')
        return json.loads(raw) if raw else None

def send(kind,content,channel='shell'):
    if client:
        msg=client.session.msg(kind,content=content);getattr(client,channel+'_channel').send(msg);return msg['header']['msg_id']
    msg_id=uuid.uuid4().hex
    message={'header':{'msg_id':msg_id,'session':session,'username':'rustpy','date':datetime.datetime.now(datetime.timezone.utc).isoformat(),'msg_type':kind,'version':'5.3'},'parent_header':{},'metadata':{},'content':content,'channel':channel,'buffers':[]}
    ws.send(json.dumps(message));return msg_id

def receive():
    if client:
        for channel in ('iopub','shell','control'):
            try:return getattr(client,'get_'+channel+'_msg')(timeout=0.01)
            except queue.Empty:pass
        return None
    try:
        raw=ws.recv()
        if isinstance(raw,bytes):raise RuntimeError('Unsupported binary Jupyter channel frame; reconnect with JSON channels')
        if len(raw)>32*1024*1024:raise RuntimeError('Jupyter frame exceeds 32 MiB')
        return json.loads(raw)
    except Exception as error:
        import websocket
        if isinstance(error,websocket.WebSocketTimeoutException):return None
        raise

def execute(code,request_id,expressions=None,silent=False,operation='exec'):
    expressions=dict(expressions or {})
    if operation=='exec':expressions['rustpy_invalidate']='_rustpy_values.changed()'
    msg=send('execute_request',dict(code=code,silent=silent,store_history=not silent,user_expressions=expressions or {},allow_stdin=False,stop_on_error=True))
    pending[msg]=(request_id,operation);return msg

def query(req,expression,operation):execute('',req['id'],{'rustpy':expression},True,operation)
def text_value(value):
    value=value.get('data',{}).get('text/plain','None')
    return ast.literal_eval(value)

def handle(message):
    global active
    kind=message.get('msg_type') or message.get('header',{}).get('msg_type');content=message.get('content',{});parent=message.get('parent_header',{}).get('msg_id')
    owner=pending.get(parent);rid,operation=owner or (0,'background')
    if kind=='status' and content.get('execution_state')=='idle':
        idle_parents.add(parent)
        if parent in execute_done:
            completed=execute_done.pop(parent);pending.pop(parent,None);idle_parents.discard(parent);emit('result',id=completed['id'],error=completed.get('error'),execution_count=completed.get('execution_count'));active=None
    elif kind in ('stream','execute_result','display_data','update_display_data','error','clear_output','comm_open','comm_msg','comm_close'):
        # Only this frontend's active execution may mutate the cell; comm updates are session-wide.
        if not owner and kind not in ('comm_open','comm_msg','comm_close'):return
        if kind=='stream':emit('stdout' if content.get('name')=='stdout' else 'stderr',text=content.get('text',''))
        else:
            payload=dict(content);payload['type']=kind
            if kind in ('display_data','execute_result','update_display_data'):
                payload.setdefault('metadata',{})['rustpy_cell_id']=cell_owners.get(parent)
                for mime in ('application/vnd.plotly.v1+json','application/vnd.vegalite.v5+json','application/vnd.vegalite.v4+json','application/vnd.bokehjs_exec.v0+json'):
                    if mime in content.get('data',{}):
                        expression='_rustpy_render_plot(%r, %r)'%(mime,content['data'][mime])
                        render_id=execute('',0,{'rustpy':expression},True,'render:'+str(cell_owners.get(parent,'')))
            if 'data' in payload and kind in ('execute_result','display_data','update_display_data'):payload['data']={k:v if isinstance(v,str) else json.dumps(v) for k,v in payload['data'].items()}
            emit('stdout',text=marker+json.dumps(payload)+'\n')
    elif kind=='complete_reply' and owner:
        pending.pop(parent,None);emit('completions',id=rid,prefix=operation.removeprefix('complete:'),items=[dict(name=item,kind='variable') for item in content.get('matches',[])][:1000])
    elif kind=='execute_reply' and owner:
        if operation=='exec':
            execute_done[parent]=dict(id=rid,error=content.get('ename') if content.get('status')=='error' else None,execution_count=content.get('execution_count'))
            if parent in idle_parents:
                completed=execute_done.pop(parent);pending.pop(parent,None);idle_parents.discard(parent);emit('result',id=rid,error=completed.get('error'),execution_count=completed.get('execution_count'));active=None
        elif operation=='startup':
            pending.pop(parent,None)
            if content.get('status')=='error':raise RuntimeError('Kernel setup failed: '+content.get('evalue',''))
            info=content.get('user_expressions',{}).get('rustpy_runtime',{})
            runtime=json.loads(text_value(info)) if info.get('status')=='ok' else {}
            emit('ready',version=runtime.get('version','Jupyter'),executable=runtime.get('executable',cfg.get('python') or 'Jupyter kernel'))
        else:
            pending.pop(parent,None)
            try:
                result=content.get('user_expressions',{}).get('rustpy',{})
                if result.get('status')!='ok':raise RuntimeError(result.get('evalue') or content.get('evalue') or 'Kernel query failed')
                value=json.loads(text_value(result))
                if operation.startswith('render:'):
                    emit('stdout',text=marker+json.dumps({'type':'display_data','data':{'text/html':value},'metadata':{'rustpy_cell_id':operation.partition(':')[2]}})+'\n')
                elif operation=='debugger':emit('debugger',id=rid,host=cfg.get('debug_host') or '127.0.0.1',port=value)
                elif operation=='vars':emit('vars',id=rid,items=value['items'])
                elif operation=='inspect':emit('children',id=rid,name=value.get('name',''),items=value.get('items',[]),error=value.get('error'))
                elif operation=='data':emit('data',id=rid,table=value.get('table'),error=value.get('error'))
            except Exception as error:
                log('Notebook query failed: '+str(error))
                emit('debugger' if operation=='debugger' else 'children' if operation=='inspect' else operation,id=rid,error=str(error),items=[],host='',port=0)

def command(req):
    global active
    op=req.get('op');rid=req.get('id',0)
    if op=='exec':
        if len(cell_owners)>4096:cell_owners.pop(next(iter(cell_owners)))
        active=execute(req['code'],rid);cell_owners[active]=req.get('cell_id')
    elif op=='complete':
        code=req['code'];cursor=req['cursor'];start=cursor
        while start and (code[start-1].isalnum() or code[start-1]=='_'):start-=1
        msg=send('complete_request',dict(code=code,cursor_pos=cursor));pending[msg]=(rid,'complete:'+code[start:cursor])
    elif op in ('vars','inspect','data'):
        payload=dict(req)
        if op=='vars':payload.update(op='vars_page',count=200,include_modules=False)
        if op=='inspect':payload.update(op='page',count=200)
        query(req,"__import__('json').dumps(_rustpy_values.dispatch(%r))"%payload,op)
    elif op=='debugger':
        if cfg['kind'] in ('remote','ssh','docker') and not cfg.get('debug_host'):emit('debugger',id=rid,host='',port=0,error='Configure a reachable debug host/SSH forwarded endpoint before debugging a remote kernel');return
        code="import debugpy as _rustpy_debugpy\n_rustpy_debug_address = globals().get('_rustpy_debug_address') or _rustpy_debugpy.listen(%r)"%((cfg.get('debug_bind') or '127.0.0.1',int(cfg.get('debug_port') or 0)),)
        execute(code,rid,{'rustpy':"__import__('json').dumps(_rustpy_debug_address[1])"},True,'debugger')
    elif op=='widget_custom':send('comm_msg',dict(comm_id=req['comm_id'],data=dict(method='custom',content=req['content'])))
    elif op=='widget':send('comm_msg',dict(comm_id=req['comm_id'],data=dict(method='update',state=req['state'],buffer_paths=[])))
    elif op=='interrupt':
        if km:km.interrupt_kernel()
        else:api('api/kernels/'+kernel_id+'/interrupt','POST',{})
    elif op=='restart':
        pending.clear();execute_done.clear();idle_parents.clear();cell_owners.clear()
        if km:km.restart_kernel(now=True);client.wait_for_ready(timeout=30)
        else:api('api/kernels/'+kernel_id+'/restart','POST',{})
        setup()
    elif op=='exit':closing.set()

def setup():
    # Kernels do not inherit the IDE console helper directory (including remote
    # kernels). Inject the captured shared adapter and pass its callable directly,
    # rather than importing a helper file from the kernel's ambient sys.path.
    source="import types as _rustpy_types\n_rustpy_scientific_module = _rustpy_types.ModuleType('rustpy_notebook_scientific')\nexec(%r, _rustpy_scientific_module.__dict__)\n_rustpy_values_module = _rustpy_types.ModuleType('rustpy_notebook_values')\nexec(%r, _rustpy_values_module.__dict__)\n_rustpy_values = _rustpy_values_module.Values(globals(), scientific_table=_rustpy_scientific_module.scientific_table)\n%%matplotlib inline"%(config['scientific_table'], config['values'])
    # Keep IPython magic on its own execution line, rather than invalid Python after imports.
    source=source.replace('\n%matplotlib inline',"\ntry:\n get_ipython().run_line_magic('matplotlib', 'inline')\nexcept ImportError:\n pass" if cfg.get('matplotlib',True) else '')
    source += "\n" + r"""
try:
    import bokeh.io as _rustpy_bokeh_io
    def _rustpy_bokeh_show(obj, *args, **kwargs):
        from bokeh.embed import file_html
        from bokeh.resources import INLINE
        from IPython.display import display, HTML
        display(HTML(file_html(obj, INLINE)))
    _rustpy_bokeh_io.show = _rustpy_bokeh_show
except ImportError:
    pass
def _rustpy_df_page(frame):
    import json
    cache=globals().setdefault('_rustpy_table_results', {})
    if len(cache)>=32:cache.pop(next(iter(cache)))
    key=str(id(frame));cache[key]=frame
    return dict(expression='_rustpy_table_results[%r]'%key,kind='dataframe',columns=[str(c) for c in frame.columns[:20]],index=[str(i) for i in frame.index[:100]],rows=[[str(v)[:256] for v in row] for row in frame.iloc[:100,:20].itertuples(index=False,name=None)],row_start=0,col_start=0,total_rows=len(frame),total_cols=len(frame.columns),truncated=len(frame)>100 or len(frame.columns)>20)
get_ipython().display_formatter.formatters['application/json'].for_type_by_name('pandas.core.frame','DataFrame',_rustpy_df_page)
def _rustpy_render_plot(mime, payload):
    import json
    if mime == 'application/vnd.plotly.v1+json':
        import plotly.io
        html = plotly.io.to_html(payload, include_plotlyjs=True, full_html=True)
    elif 'vegalite' in mime:
        import altair
        html = altair.Chart.from_dict(payload).to_html(inline=True)
    elif 'bokeh' in mime:
        from bokeh.document import Document
        from bokeh.embed import file_html
        from bokeh.resources import INLINE
        document = payload.get('doc') or next(iter(payload.get('docs_json', {}).values()), None)
        if document is None: raise ValueError('Bokeh payload has no document; display file_html(..., INLINE) for an interactive output')
        html = file_html(Document.from_json(document), INLINE)
    else: raise ValueError('Unknown plot MIME')
    return json.dumps(html)
"""
    execute(source,0,{'rustpy_runtime':"__import__('json').dumps({'version':__import__('sys').version.split()[0], 'executable':__import__('sys').executable})"},silent=True,operation='startup')

def input_loop():
    for line in sys.stdin:
        if len(line)>4*1024*1024:log('Notebook request exceeds 4 MiB');closing.set();break
        try:
            command=json.loads(line)
            if command.get('op')=='exit':
                closing.set()
                if km:
                    try:km.shutdown_kernel(now=True)
                    except Exception:pass
                if server:server.terminate()
                return
            commands.put(command,timeout=1)
        except Exception as error:log(str(error));break
    commands.put({'op':'exit'})
threading.Thread(target=input_loop,daemon=True).start()
try:
    if cfg['kind'] in ('local','ssh','docker'):
        from jupyter_client import KernelManager
        from jupyter_client.kernelspec import KernelSpecManager
        emit('stdout',text=marker+json.dumps({'type':'kernel_specs','names':list(KernelSpecManager().find_kernel_specs())})+'\n')
        if closing.is_set():raise InterruptedError('Notebook connection cancelled')
        km=KernelManager(kernel_name='python3' if cfg.get('kernel')=='project' else cfg.get('kernel') or 'python3')
        if cfg.get('kernel')=='project':km.kernel_spec.argv=[sys.executable,'-m','ipykernel_launcher','-f','{connection_file}']
        km.start_kernel(cwd=os.getcwd());
        if closing.is_set():raise InterruptedError('Notebook connection cancelled')
        client=km.blocking_client();client.start_channels();client.wait_for_ready(timeout=30)
    else:
        if cfg['kind']=='managed':
            import socket
            sock=socket.socket();sock.bind(('127.0.0.1',0));port=sock.getsockname()[1];sock.close()
            cfg['token']=secrets.token_urlsafe(32);base='http://127.0.0.1:%d/'%port
            server=subprocess.Popen([sys.executable,'-m','jupyter_server','--no-browser','--ServerApp.ip=127.0.0.1','--ServerApp.port=%d'%port,'--ServerApp.port_retries=0','--IdentityProvider.token='+cfg['token']]+(['--allow-root'] if hasattr(os,'geteuid') and os.geteuid()==0 else []),stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
            def logs():
                for line in server.stdout:log(line.replace(cfg['token'],'[token]'))
            threading.Thread(target=logs,daemon=True).start()
            ready_deadline=time.monotonic()+30
            for _ in range(150):
                if closing.is_set():raise InterruptedError('Notebook server start cancelled')
                if time.monotonic()>ready_deadline:raise TimeoutError('Managed server start exceeded 30 seconds')
                if server.poll() is not None:raise RuntimeError('Managed server exited; install jupyter_server/ipykernel or inspect server log')
                try:api('api',timeout=1);break
                except Exception:time.sleep(0.1)
            else:raise RuntimeError('Managed Jupyter server did not become ready')
            emit('stdout',text=marker+json.dumps({'type':'server_info','url':base,'token':cfg['token']})+'\n')
        if cfg.get('password'):
            from http import cookiejar
            jar=cookiejar.CookieJar();context=ssl.create_default_context(cafile=cfg.get('ca_file') or None)
            if not cfg.get('verify_ssl',True):context.check_hostname=False;context.verify_mode=ssl.CERT_NONE
            opener=http.build_opener(http.HTTPCookieProcessor(jar),http.HTTPSHandler(context=context),SafeRedirect())
            opener.open(base+'login',timeout=15).read(1024*1024)
            xsrf=next((c.value for c in jar if c.name=='_xsrf'),'')
            opener.open(http.Request(base+'login',data=urlparse.urlencode({'password':cfg['password'],'_xsrf':xsrf}).encode()),timeout=15).read(1024*1024)
            cookie='; '.join(c.name+'='+c.value for c in jar)
        kernels=api('api/kernelspecs');emit('stdout',text=marker+json.dumps({'type':'kernel_specs','names':list(kernels.get('kernelspecs',{}))})+'\n')
        if closing.is_set():raise InterruptedError('Notebook connection cancelled')
        session_attempted=True
        model=api('api/sessions','POST',{'name':server_session_name,'path':server_session_name+'.ipynb','type':'notebook','kernel':{'name':cfg.get('kernel') or kernels.get('default','python3')}})
        server_session_id=model['id'];kernel_id=model['kernel']['id']
        log('Owned Jupyter session: '+server_session_name)
        if closing.is_set():raise InterruptedError('Notebook connection cancelled')
        import websocket
        endpoint=base.replace('https:','wss:',1).replace('http:','ws:',1)+'api/kernels/'+kernel_id+'/channels?session_id='+session
        headers=['Authorization: token '+cfg['token']] if cfg.get('token') else []
        if cookie:headers.append('Cookie: '+cookie)
        sslopt={'cert_reqs':ssl.CERT_REQUIRED if cfg.get('verify_ssl',True) else ssl.CERT_NONE}
        if cfg.get('ca_file'):sslopt['ca_certs']=cfg['ca_file']
        ws=websocket.create_connection(endpoint,header=headers,timeout=15,sslopt=sslopt,origin=base.rstrip('/'));ws.settimeout(0.05)
    signal.signal(signal.SIGTERM,lambda *_:closing.set())
    setup()
    while not closing.is_set():
        while True:
            try:command(commands.get_nowait())
            except queue.Empty:break
        message=receive()
        if message:handle(message)
        if km and time.monotonic()-last_resource>2:
            last_resource=time.monotonic()
            try:
                pid=km.provisioner.pid
                with open('/proc/%d/statm'%pid) as file:memory=int(file.read().split()[1])*os.sysconf('SC_PAGE_SIZE')
                with open('/proc/%d/stat'%pid) as file:fields=file.read().rpartition(') ')[2].split();cpu=(int(fields[11])+int(fields[12]))/os.sysconf('SC_CLK_TCK')
                emit('stdout',text=marker+json.dumps({'type':'resources','memory_bytes':memory,'cpu_seconds':cpu})+'\n')
            except (OSError,AttributeError,ValueError):pass
except BaseException as error:log('Jupyter: '+str(error)+'\n'+traceback.format_exc(limit=5))
finally:
    closing.set()
    if client:client.stop_channels()
    if km:
        try:km.shutdown_kernel(now=True)
        except Exception:pass
    if session_attempted:
        try:
            if not server_session_id:
                matches=[item for item in api('api/sessions') if item.get('name')==server_session_name or item.get('path')==server_session_name+'.ipynb']
                if matches:server_session_id=matches[0]['id']
            if server_session_id:api('api/sessions/'+server_session_id,'DELETE')
        except Exception as error:log('Could not confirm cleanup of owned Jupyter session '+server_session_name+': '+str(error))
    elif kernel_id:
        try:api('api/kernels/'+kernel_id,'DELETE')
        except Exception as error:log('Could not confirm kernel cleanup: '+str(error))
    if ws:ws.close()
    if server:
        server.terminate()
        try:server.wait(timeout=2)
        except subprocess.TimeoutExpired:server.kill();server.wait()
    emit('exited')
