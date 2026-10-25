import re,sys
res=[];cur=[]
for l in open(sys.argv[1]).read().split('\n'):
    if l.startswith('=='):
        res.append((l,cur));cur=[]
    else: cur.append(l)
tabs=[]
for name,ls in res:
    d={}
    for l in ls:
        m=re.match(r'STAGE (.+?)\s+n=\s*(\d+)\s+p50=\s*([\d.]+) p95=\s*([\d.]+) p99=\s*([\d.]+)',l)
        if m: d[m.group(1)]=(m.group(3),m.group(4),m.group(5))
        if l.startswith('PX-6'):
            g=lambda k:re.search(k+r'=([\d.]+)',l).group(1)
            d['_PX']=(re.search(r'on_us p50=([\d.]+) p99=([\d.]+)',l).groups(), g('overall'), g('min_plugin'), g('proc_cpu_pct_of_core'), g('load1'))
    tabs.append((name,d))
print("conditions:")
for n,d in tabs: print(" ",n,d.get('_PX'))
keys=sorted(k for k in tabs[0][1] if not k.startswith('_'))
print("%-28s %s"%("stage p50/p95/p99 us"," | ".join(n[3:20] for n,_ in tabs)))
for k in keys:
    print("%-28s %s"%(k," | ".join("/".join(t[1].get(k,('-','-','-'))) for t in tabs)))
