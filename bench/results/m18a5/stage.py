import re,sys
want=['TOTAL pre-send','TOTAL turn','k1 pick-up (cold)','k1 pick-up (hot)','k4 guest call ','w7 send waited','w1 snapshot']
buf=[]
print("%-24s"%"run"+"".join("%-17s"%w[:16] for w in want))
for f in sys.argv[1:]:
  for l in open(f):
    if l.startswith('STAGE'): buf.append(l)
    elif l.startswith('=='):
      tag=' '.join(l.split()[1:])
      out=[]
      for w in want:
        v='-'
        for b in buf:
          if b.startswith('STAGE '+w):
            m=re.search(r'n=\s*(\d+)\s+p50=\s*([\d.]+) p95=\s*([\d.]+)',b); v='%s/%s(%s)'%(int(float(m.group(2))),int(float(m.group(3))),m.group(1)); break
        out.append("%-17s"%v)
      print("%-24s"%tag+"".join(out)); buf=[]
    elif l.startswith('PX-6'): pass
