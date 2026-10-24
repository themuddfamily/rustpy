#!/usr/bin/env python3
"""Generate scalar-safe primary AST oracles from a validated existing release corpus."""
import argparse, ast, gzip, hashlib, json, os, pathlib, sys, warnings
P = pathlib.Path

PROTOCOL = {'name':'v2-surrogate-codepoints','ast':'Recursive ast._fields; omit _attributes positions; exec/type_comments=False','literals':'bytes/complex/float/Ellipsis literal_type+CPython repr; strings containing surrogate codepoints literal_type=str/codepoints; ordinary strings unchanged'}
def sha(p):
    with p.open('rb') as f: return hashlib.file_digest(f,'sha256').hexdigest()
def normalize(v):
    if isinstance(v,ast.AST):return {'node':type(v).__name__,'fields':{k:normalize(getattr(v,k)) for k in v._fields}}
    if isinstance(v,list):return [normalize(x) for x in v]
    if isinstance(v,str) and any(0xD800<=ord(c)<=0xDFFF for c in v):return {'literal_type':'str','codepoints':[ord(c) for c in v]}
    if isinstance(v,(str,int,bool)) or v is None:return v
    if isinstance(v,(bytes,complex,float)) or v is Ellipsis:return {'literal_type':type(v).__name__,'repr':repr(v)}
    raise TypeError(type(v).__name__)
def downgrade_v2(v):
    if isinstance(v,dict) and v.get('literal_type')=='str' and set(v)=={'literal_type','codepoints'}:
        return ''.join(chr(c) for c in v['codepoints'])
    if isinstance(v,list):return [downgrade_v2(x) for x in v]
    if isinstance(v,dict):return {k:downgrade_v2(x) for k,x in v.items()}
    return v
def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--baseline', type=P, required=True, help='prepare_release.py output directory')
    ap.add_argument('--output', type=P, required=True)
    ap.add_argument('--stdlib-root', type=P, help='Explicit relocated Lib root; every path/hash still checked')
    args = ap.parse_args()
    OLD = args.baseline.resolve(strict=True)
    OUT = args.output.resolve()
    assert not sys.flags.optimize
    assert sys.implementation.name=='cpython' and sys.version_info[:3]==(3,12,3)
    assert sys.get_int_max_str_digits() == 4300, 'CPython default decimal admission limit required'
    manifest_path=OLD/'manifest.json'; old_sha=sha(manifest_path); old=json.loads(manifest_path.read_text())
    assert old['schema'] == 1 and old['release_version'] == '3.12.3'
    assert old['archive_sha256'] == '56bfef1fdfc1221ce6720e43a661e3eb41785dd914ce99698d8c7896af4bdaa1'
    assert old['complete_release_lib_py'] is True and old['test_module_count'] == 1019
    assert old['module_count']==1774 and old['accepted_count']==1765 and old['rejected_count']==9
    assert sha(P(sys.executable))==old['interpreter_sha256']
    oracle_old=OLD/'oracles.jsonl.gz'; assert sha(oracle_old)==old['oracle_sha256']
    assert sha(OLD/'Python-3.12.3.tar.xz')==old['archive_sha256']
    root=(args.stdlib_root or P(old['stdlib_root'])).resolve(strict=True);assert sorted(str(p.relative_to(root)) for p in root.rglob('*.py') if p.is_file())==[x['path'] for x in old['files']]
    OUT.mkdir(mode=0o700,exist_ok=False)
    oracle=OUT/'oracles.jsonl.gz';accepted=rejected=uncompressed=warning_count=0; rejected_files=[]; kinds={};changed=[]
    with oracle_old.open('rb') as old_raw, gzip.GzipFile(fileobj=old_raw,mode='rb') as old_stream, oracle.open('xb') as raw:
        with gzip.GzipFile(filename='',fileobj=raw,mode='wb',mtime=0) as stream:
            for item in old['files']:
                path=root/item['path']; data=path.read_bytes();assert len(data)==item['bytes'] and hashlib.sha256(data).hexdigest()==item['source_sha256']
                previous_line=old_stream.readline();previous=json.loads(previous_line);assert previous['source']==item
                with warnings.catch_warnings(record=True) as emitted:
                    warnings.simplefilter('always')
                    try:
                        tree=ast.parse(data,filename=item['path'],mode='exec',type_comments=False)
                        for node in ast.walk(tree):kinds[type(node).__name__]=kinds.get(type(node).__name__,0)+1
                        value={'accepted':True,'ast':normalize(tree)};accepted+=1
                    except (SyntaxError,UnicodeError,ValueError) as error:
                        value={'accepted':False,'error_type':type(error).__name__};rejected+=1;rejected_files.append({'path':item['path'],'error_type':type(error).__name__})
                    warning_count+=len(emitted)
                old_protocol_line=(json.dumps({'source':item,'oracle':downgrade_v2(value)},ensure_ascii=True,separators=(',',':'))+'\n').encode('ascii')
                assert old_protocol_line==previous_line,item['path']
                if value!=previous['oracle']:changed.append(item['path'])
                line=(json.dumps({'source':item,'oracle':value},ensure_ascii=True,separators=(',',':'))+'\n').encode('ascii');uncompressed+=len(line);stream.write(line);stream.flush()
                assert raw.tell()<12*1024*1024
                assert sha(path)==item['source_sha256']
            assert not old_stream.readline()
        raw.flush();os.fsync(raw.fileno())
    assert (accepted,rejected)==(1765,9) and rejected_files==old['rejected_files'] and kinds==old['ast_node_kinds']
    assert sha(manifest_path)==old_sha and sha(oracle_old)==old['oracle_sha256']
    descriptor=json.dumps(PROTOCOL,sort_keys=True,separators=(',',':')).encode('ascii')
    manifest=dict(old,stdlib_root=str(root),corpus_root=str(root),oracle_sha256=sha(oracle),oracle_uncompressed_bytes=uncompressed,oracle_compressed_bytes=oracle.stat().st_size,warnings_count=warning_count,literal_normalization_protocol=PROTOCOL['name'],normalization=PROTOCOL['ast']+'; '+PROTOCOL['literals'],protocol_descriptor=PROTOCOL,protocol_descriptor_sha256=hashlib.sha256(descriptor).hexdigest(),original_manifest_sha256=old_sha,original_oracle_sha256=old['oracle_sha256'],generation_script_sha256=sha(P(__file__)),protocol_changed_modules=changed,corpus_reused_without_extraction=True,rust_execution=False)
    with (OUT/'manifest.json').open('x') as f:json.dump(manifest,f,indent=2);f.write('\n');f.flush();os.fsync(f.fileno())
    print(json.dumps({'output':str(OUT),'manifest_sha256':sha(OUT/'manifest.json'),'oracle_sha256':sha(oracle),'counts':[len(old['files']),accepted,rejected],'compressed_bytes':oracle.stat().st_size,'protocol_changed_modules':changed},indent=2))
if __name__=='__main__':main()
