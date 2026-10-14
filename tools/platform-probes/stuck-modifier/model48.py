RD=500
def sim(hold, interval=30, first_delay=500, fixed=False):
    st=dict(last=0,lt=0,saw=False,pm=set()); kp=set(); INS=1
    def fc(ks,t,state):
        is_press = state!=0
        same= ks==st['last']; soon=(t-st['lt'])<RD
        st['last']=ks; st['lt']=t
        if not is_press and ks in st['pm']:
            st['pm'].discard(ks); return False
        if same and soon:
            if is_press and st['saw']:
                st['saw']=False; st['pm'].add(ks); return False
            st['saw']=True; return True
        st['saw']=False; return True
    def ev(t,state):
        ign = False if (fixed and state==2) else fc(INS,t,state)
        released = state==0
        if not ign: return        # grabbed modifier, not ignorable -> early return (reaches app)
        if released: kp.discard(INS)
        else: kp.add(INS)
    ev(0,1)
    t=first_delay
    while t<hold*1000: ev(t,2); t+=interval
    ev(hold*1000,0)
    return sorted(kp)
for h in (0.3,0.45,0.55,0.7,0.9,1.5):
    print(h,"48.7 stale kp:",sim(h)," fixed:",sim(h,fixed=True))
