import re,sys,statistics as st
rows=[]
for f in sys.argv[1:]:
    cur=None
    for l in open(f):
        if l.startswith('PX-6'):
            cur=l
        elif l.startswith('==') and cur:
            m=re.match(r'== (\S+) (\S+) (\S+) (\d+)ms',l)
            b,sch,tier,sp=m.groups()
            g=lambda k:float(re.search(k+r'=([\d.]+)',cur).group(1))
            pp=eval(re.search(r'per_plugin=(\[[^\]]*\])',cur).group(1))
            p=re.search(r'on_us p50=([\d.]+) p99=([\d.]+)',cur).groups()
            rows.append((tier,sp,sch,b+('+warm' if 'm2warm' in f else ''),float(p[0]),float(p[1]),g('overall'),sum(pp)/len(pp),min(pp),g('proc_cpu_pct_of_core'),g('load1')))
            cur=None
rows.sort(key=lambda r:(r[0],int(r[1]),r[2],r[3]))
print("tier  keyms sched  bin        p50   p99  all10 mean  min   cpu%  load1")
for r in rows: print("%-5s %4s %-6s %-10s %5.0f %5.0f %5.3f %5.3f %5.3f %5.1f %5.1f"%(r[0],r[1],r[2],r[3],*r[4:]))
