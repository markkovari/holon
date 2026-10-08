import json,random,http.client,base64,time,math,sys,threading,subprocess
from concurrent.futures import ThreadPoolExecutor
PORT=int(sys.argv[1]); ENGINE=sys.argv[2]; PHASES=sys.argv[3].split(","); OUT=open(sys.argv[4],"a")
D=256
import os
def _tok():
    c=http.client.HTTPConnection("localhost",PORT,timeout=30)
    c.request("POST","/signin",b'{"user":"root","pass":"root"}',{"Accept":"application/json"})
    return json.loads(c.getresponse().read())["token"]
AUTH_MODE=os.environ.get("AUTH","bearer")
AUTH=("Bearer "+_tok()) if AUTH_MODE=="bearer" else "Basic "+base64.b64encode(b"root:root").decode()
tl=threading.local()
def conn():
    c=getattr(tl,"c",None)
    if c is None: c=tl.c=http.client.HTTPConnection("localhost",PORT,timeout=900)
    return c
def q(sql,db,ns="b"):
    for attempt in range(3):
        try:
            c=conn(); c.request("POST","/sql",sql.encode(),{"Accept":"application/json","surreal-ns":ns,"surreal-db":db,"Authorization":AUTH})
            r=c.getresponse(); body=r.read()
            if r.status!=200: raise RuntimeError(f"http {r.status}")
            res=json.loads(body)
            for x in res:
                if x.get("status")=="ERR": raise RuntimeError(x["result"])
            return res
        except (http.client.HTTPException,ConnectionError,OSError):
            tl.c=None
            if attempt==2: raise
def emit(**k): 
    k["engine"]=ENGINE; k["auth"]=AUTH_MODE; OUT.write(json.dumps(k)+"\n"); OUT.flush(); print(json.dumps(k),flush=True)
def pct(xs,p): xs=sorted(xs); return xs[min(len(xs)-1,int(len(xs)*p/100))]
def stats(xs): return dict(p50=round(pct(xs,50),2),p95=round(pct(xs,95),2),p99=round(pct(xs,99),2),mean=round(sum(xs)/len(xs),2))
R=random.Random(7)
def gauss_vec(c,noise,rng): 
    v=[c[j]+rng.gauss(0,noise) for j in range(D)]; n=math.sqrt(sum(x*x for x in v)); return [round(x/n,4) for x in v]
CENT=[[random.Random(1000+i).gauss(0,1) for _ in range(D)] for i in range(50)]
def mkdb(db):
    q("DEFINE NAMESPACE IF NOT EXISTS b; USE NS b; DEFINE DATABASE IF NOT EXISTS %s;"%db,"","")
    q("REMOVE TABLE IF EXISTS memory;",db)
def mkidx(db,hnsw=True,extra=""):
    s="DEFINE INDEX memory_scope ON memory FIELDS scope;"
    if hnsw: s+=f"DEFINE INDEX memory_vec ON memory FIELDS vec HNSW DIMENSION {D} DIST COSINE {extra};"
    q(s,db)
def load(db,plan,threads=4,batch=100,tag=""):
    """plan: list of (scope, count). Returns per-batch latencies."""
    rows=[]
    for sc,cnt in plan:
        ci=int(sc[1:]) % 50 if sc[1:].isdigit() else abs(hash(sc))%50
        rows+= [(sc,ci)]*cnt
    random.Random(3).shuffle(rows)
    chunks=[rows[i:i+batch] for i in range(0,len(rows),batch)]
    lat=[]; lk=threading.Lock(); t0=time.time(); done=[0]
    def job(ch):
        rng=random.Random(hash((ch[0],len(ch),time.time())))
        body=",".join(json.dumps({"scope":s,"text":f"note about {s}","vec":gauss_vec(CENT[ci],0.8,rng)}) for s,ci in ch)
        t=time.perf_counter(); q(f"INSERT INTO memory [{body}];",db); d=(time.perf_counter()-t)*1000
        with lk: lat.append(d); done[0]+=len(ch)
    with ThreadPoolExecutor(threads) as ex: list(ex.map(job,chunks))
    el=time.time()-t0
    return dict(rows=len(rows),secs=round(el,1),rows_per_s=round(len(rows)/el),batch_ms=stats(lat))
def qvec(scope,rng):
    ci=int(scope[1:])%50 if scope[1:].isdigit() else abs(hash(scope))%50
    return json.dumps(gauss_vec(CENT[ci],0.8,rng))
def ids(res): return {x["id"] for x in res[-1]["result"]}
def timed(sql,db,reps=40):
    ts=[]
    for _ in range(reps):
        t=time.perf_counter(); q(sql,db); ts.append((time.perf_counter()-t)*1000)
    return ts
def probe(db,scope,k=10,nq=30,ef=None,label=""):
    rng=random.Random(11); rec=[];got=[];tk=[];te=[]
    knn=f"<|{k},{ef}|>" if ef else f"<|{k},COSINE|>"
    # exact via scope index
    for _ in range(nq):
        v=qvec(scope,rng)
        t=time.perf_counter(); a=q(f"SELECT id FROM memory WHERE vec {knn} {v} AND scope='{scope}';",db); tk.append((time.perf_counter()-t)*1000)
        t=time.perf_counter(); b=q(f"SELECT id,vector::similarity::cosine(vec,{v}) AS s FROM memory WHERE scope='{scope}' ORDER BY s DESC LIMIT {k};",db); te.append((time.perf_counter()-t)*1000)
        A,B=ids(a),ids(b); got.append(len(A)); rec.append(len(A&B)/max(1,len(B)))
    emit(exp=label,scope=scope,k=k,ef=ef,knn=stats(tk),exact_scan=stats(te),rows_returned_mean=round(sum(got)/len(got),2),recall_vs_exact=round(sum(rec)/len(rec),3))

if "base" in PHASES:
    mkdb("base"); ts=timed("RETURN 1;","base",300); emit(exp="http_roundtrip_return1",**stats(ts))
if "scale" in PHASES:
    for N in (1000,10000,50000,100000,200000):
        db=f"scale{N}"; mkdb(db); mkidx(db)
        info=load(db,[(f"p{i}",N//10) for i in range(10)]); emit(exp="load_uniform10",N=N,**info)
        probe(db,"p3",label=f"scale_N{N}")
        v=qvec("p3",random.Random(5))
        emit(exp=f"scale_N{N}_unfiltered",knn=stats(timed(f"SELECT id FROM memory WHERE vec <|10,COSINE|> {v};",db,25)))
        emit(exp=f"scale_N{N}_scope_only_no_vec",q=stats(timed(f"SELECT id FROM memory WHERE scope='p3' LIMIT 10;",db,25)))
if "skew" in PHASES:
    db="skew"; mkdb(db); mkidx(db)
    plan=[("big",90000),("mid",5000),("small",500),("tiny",20)]+[(f"x{i}",100) for i in range(45)]
    emit(exp="load_skew",**load(db,plan))
    for sc in ("big","mid","small","tiny"):
        probe(db,sc,k=10,label="skew")
    for ef in (40,100,200,400):
        for sc in ("small","tiny"): probe(db,sc,k=10,ef=ef,label="skew_ef")
if "conc" in PHASES:
    db="scale100000"
    rng0=random.Random(1)
    def reader(_):
        rng=random.Random(threading.get_ident()); ts=[]; end=time.time()+8
        while time.time()<end:
            v=qvec(f"p{rng.randrange(10)}",rng); sc=f"p{rng.randrange(10)}"
            t=time.perf_counter(); q(f"SELECT id FROM memory WHERE vec <|5,COSINE|> {v} AND scope='{sc}';",db); ts.append((time.perf_counter()-t)*1000)
        return ts
    for th in (1,4,16,32):
        t0=time.time()
        with ThreadPoolExecutor(th) as ex: allts=[x for r in ex.map(reader,range(th)) for x in r]
        emit(exp="concurrent_read",threads=th,qps=round(len(allts)/(time.time()-t0)),**stats(allts))
    # mixed: 8 readers while 4 writers insert
    stop=time.time()+10; wl=[];rl=[]; lk=threading.Lock()
    def writer(i):
        rng=random.Random(100+i)
        while time.time()<stop:
            body=",".join(json.dumps({"scope":"p1","text":"w","vec":gauss_vec(CENT[1],0.8,rng)}) for _ in range(10))
            t=time.perf_counter(); q(f"INSERT INTO memory [{body}];",db); 
            with lk: wl.append((time.perf_counter()-t)*1000)
    def mreader(i):
        rng=random.Random(200+i)
        while time.time()<stop:
            v=qvec("p1",rng); t=time.perf_counter(); q(f"SELECT id FROM memory WHERE vec <|5,COSINE|> {v} AND scope='p1';",db)
            with lk: rl.append((time.perf_counter()-t)*1000)
    with ThreadPoolExecutor(12) as ex:
        fs=[ex.submit(writer,i) for i in range(4)]+[ex.submit(mreader,i) for i in range(8)]; [f.result() for f in fs]
    emit(exp="mixed_8r_4w_10s",read=stats(rl),read_qps=round(len(rl)/10),write_batch10=stats(wl),write_rows_per_s=round(len(wl)*10/10))
if "nohnsw" in PHASES:
    # is HNSW needed at all? scope index + exact scan, per scope size
    db="nohnsw"; mkdb(db); mkidx(db,hnsw=False)
    plan=[("big",90000),("mid",5000),("small",500),("tiny",20)]+[(f"x{i}",100) for i in range(45)]
    emit(exp="load_skew_no_hnsw",**load(db,plan))
    for sc in ("big","mid","small","tiny"):
        rng=random.Random(2); ts=[]
        for _ in range(20):
            v=qvec(sc,rng); t=time.perf_counter(); q(f"SELECT id,vector::similarity::cosine(vec,{v}) AS s FROM memory WHERE scope='{sc}' ORDER BY s DESC LIMIT 10;",db); ts.append((time.perf_counter()-t)*1000)
        emit(exp="nohnsw_exact_scan",scope=sc,**stats(ts))

if "auth" in PHASES:
    # as-built cost of Basic vs token on the realistic query, same data
    db="scale100000"
    for mode in ("basic","bearer"):
        global_auth="Basic "+base64.b64encode(b"root:root").decode() if mode=="basic" else "Bearer "+_tok()
        AUTH=global_auth
        v=qvec("p3",random.Random(5)); tl.c=None
        ts=timed(f"SELECT id FROM memory WHERE vec <|5,COSINE|> {v} AND scope='p3';",db,40)
        emit(exp="auth_cost_filtered_knn",mode=mode,**stats(ts))
