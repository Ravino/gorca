import socket, time, statistics, sys
s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.settimeout(2)
buf=b""
def rl():
    global buf
    while b"\r\n" not in buf: buf+=s.recv(65536)
    l,buf=buf.split(b"\r\n",1); return l.decode()
def cmd(c):
    s.sendall((c+"\r\n").encode())
    while True:
        l=rl()
        if not l.startswith("7") and l[3:4]==' ': return l
def speak(text,prio):
    cmd(f"SET SELF PRIORITY {prio}"); s.sendall(b"SPEAK\r\n")
    while True:
        l=rl()
        if l.startswith("230"): break
    s.sendall((text+"\r\n.\r\n").encode())
    while True:
        l=rl()
        if l.startswith("225 ") : return time.perf_counter()
def drain(t=0.4):
    global buf
    s.settimeout(t); ev=[]
    try:
        while True: ev.append((time.perf_counter(),rl()))
    except Exception: pass
    s.settimeout(2); return ev
cmd("SET SELF CLIENT_NAME t:g:m"); cmd("SET SELF NOTIFICATION ALL on")
LONG="This is a long sentence that keeps going on and on. "*8
def scenario(p1,p2,gap=0.15):
    drain(1.0)
    speak(LONG,p1); time.sleep(gap)
    t=speak("b",p2)
    ev=drain(1.5)
    return [(round((tt-t)*1e3,1),l.split(' ',1)[0]+' '+l.split(' ',3)[-1][:12]) for tt,l in ev if l.startswith('7')][:10]
for p1,p2 in [("TEXT","TEXT"),("IMPORTANT","IMPORTANT"),("TEXT","IMPORTANT"),("MESSAGE","TEXT"),("NOTIFICATION","TEXT")]:
    print(p1,"then",p2,":",scenario(p1,p2))
# explicit cancel then speak (screen-reader pattern) with TEXT
drain(1.0); lat=[]
for i in range(20):
    speak(LONG,"TEXT"); time.sleep(0.12)
    t0=time.perf_counter(); s.sendall(b"CANCEL SELF\r\n"); s.sendall(b"SET SELF PRIORITY TEXT\r\n")
    t=speak("b","TEXT")
    ev=drain(0.5)
    bs=[tt for tt,l in ev if "BEGIN" in l]
    # find last begin after t0
    lat.append((bs[-1]-t0)*1e3 if bs else float('nan'))
lat.sort(); print("CANCEL+SPEAK(TEXT) -> BEGIN of new msg ms: p50=%.1f max=%.1f"%(statistics.median(lat),lat[-1]))
