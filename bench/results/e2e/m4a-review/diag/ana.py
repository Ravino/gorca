import sys
for p in sys.argv[1:]:
    K=[];E=[]
    for l in open(p):
        if l.startswith('#'): continue
        f=l.rstrip('\n').split('\t')
        if f[0]=='K': K.append((int(f[1]),int(f[2]),f[3]))
        elif f[0]=='S': E.append((int(f[1]),'S',f[3]))
        elif f[0]=='C': E.append((int(f[1]),'C',f[2]+' '+f[3]))
    E.sort()
    ts=[k[1] for k in K]+[2**63]
    print(p, 'keys',len(K),'speaks',sum(1 for e in E if e[1]=='S'),'cancels',sum(1 for e in E if e[1]=='C'))
    texts={}
    for i,(idx,t,kind) in enumerate(K):
        w=[e for e in E if ts[i]<=e[0]<ts[i+1]]
        if not any(e[1]=='S' for e in w):
            print(' LOST key',i,kind,'gap to next key %.2f ms'%((ts[i+1]-t)/1e6))
            for j in range(max(0,i-2),min(len(K),i+4)):
                ww=[e for e in E if ts[j]<=e[0]<ts[j+1]]
                print('   key',j,K[j][2],'t=+%.2f'%((ts[j]-t)/1e6),[(e[1],round((e[0]-ts[j])/1e6,2),e[2]) for e in ww])
