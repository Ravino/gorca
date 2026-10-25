import re,sys
rows=[];cur=None
for f in sys.argv[1:]:
  for l in open(f):
    if l.startswith('PX-6'): cur=l
    elif l.startswith('==') and cur:
      _,var,sch,tier,sp=l.split(); sp=sp[:-2]
      g=lambda k:float(re.search(k+r'=([\d.]+)',cur).group(1))
      pp=eval(re.search(r'per_plugin=(\[[^\]]*\])',cur).group(1))
      p=re.search(r'on_us p50=([\d.]+) p99=([\d.]+)',cur).groups()
      d=re.search(r'turn_displaced=(\d+) turn_on_fast=(\d+)',cur)
      rows.append((sch,int(sp),var,float(p[0]),float(p[1]),g('overall'),sum(pp)/len(pp),min(pp),g('proc_cpu_pct_of_core'),g('load1'),d.groups() if d else ('-','-')))
      cur=None
rows.sort(key=lambda r:(r[0],r[1],r[2]))
print("sched keyms var        p50    p99   all10 mean  min    cpu%   load1 disp/onfast")
for r in rows: print("%-6s %4d %-10s %6.0f %6.0f %5.3f %5.3f %5.3f %6.1f %5.1f %s/%s"%(*r[:10],*r[10]))
