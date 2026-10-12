import socket, time, sys, statistics
sock_path=sys.argv[1]
s=socket.socket(socket.AF_UNIX); s.connect(sock_path); s.settimeout(5)
buf=b""
def readline():
    global buf
    while b"\r\n" not in buf:
        buf+=s.recv(65536)
    l,buf=buf.split(b"\r\n",1); return l.decode()
def cmd(c):
    s.sendall((c+"\r\n").encode())
    out=[]
    while True:
        l=readline()
        out.append(l)
        if len(l)>=4 and l[3]==' ' and l[0] in "2456": 
            if l[0]=="7": continue
            return out
def wait_event(prefix, timeout=3):
    end=time.perf_counter()+timeout
    while time.perf_counter()<end:
        l=readline()
        if l.startswith("7"):
            if prefix in l: return time.perf_counter(), l
    return None,None
print(cmd("SET SELF CLIENT_NAME test:gorca:main"))
print(cmd("SET SELF NOTIFICATION ALL on"))
print(cmd("SET SELF LANGUAGE en"))
print(cmd("SET SELF RATE 100"))
def speak(text, prio="IMPORTANT"):
    cmd(f"SET SELF PRIORITY {prio}")
    t0=time.perf_counter()
    s.sendall(b"SPEAK\r\n")
    l=readline()      # 230 OK RECEIVING DATA
    t_recv=time.perf_counter()
    s.sendall((text+"\r\n.\r\n").encode())
    resp=[]
    while True:
        l=readline()
        if l.startswith("7"): 
            continue
        resp.append(l)
        if l[3]==' ': break
    t_queued=time.perf_counter()
    return t0,t_recv,t_queued,resp
# warm up
speak("warm up"); time.sleep(1.0); 
# drain events
s.settimeout(0.3)
try:
    while True: readline()
except Exception: pass
s.settimeout(5)
begins=[];queued=[]
for i in range(30):
    t0,tr,tq,resp=speak("a")
    tb,l=wait_event("BEGIN")
    tend,l2=wait_event("END",2)
    begins.append((tb-t0)*1e3 if tb else float('nan')); queued.append((tq-t0)*1e3)
    time.sleep(0.05)
begins.sort(); queued.sort()
print(f"SPEAK 'a' -> queued reply: p50={statistics.median(queued):.2f} max={max(queued):.2f} ms")
print(f"SPEAK 'a' -> BEGIN event : min={begins[0]:.1f} p50={statistics.median(begins):.1f} p90={begins[int(len(begins)*0.9)]:.1f} max={begins[-1]:.1f} ms")
# long text then cancel
for prio in ["TEXT","IMPORTANT"]:
    res=[]
    for i in range(10):
        speak("This is a long sentence that keeps going on and on so that cancellation has something to do. "*5)
        wait_event("BEGIN")
        t0=time.perf_counter()
        s.sendall(b"CANCEL SELF\r\n")
        # wait for ack and STOP event
        ack=None; ts=None
        end=time.perf_counter()+3
        while time.perf_counter()<end and not (ack and ts):
            l=readline()
            if l.startswith("7") and ("STOP" in l or "CANCEL" in l or "END" in l) and not ts: ts=time.perf_counter()-t0; ev=l
            elif l[:3] in("201","200") or l[3:4]==' ': ack=time.perf_counter()-t0
        res.append((ack*1e3 if ack else float('nan'), ts*1e3 if ts else float('nan')))
        time.sleep(0.05)
    print("CANCEL ack ms p50=%.2f max=%.2f ; STOPPED event ms p50=%.2f max=%.2f"%(statistics.median(r[0] for r in res),max(r[0] for r in res),statistics.median(r[1] for r in res),max(r[1] for r in res)), "last event:",ev)
    break
