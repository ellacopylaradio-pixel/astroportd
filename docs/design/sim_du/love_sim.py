"""Simulation à agents d'une UPlanet G1N² (le LOVE), règles de DU_G1N2.md.

Géographie : 1 REGION, 3 SECTOR, 9 UMAP de 6 à 40 résidents (SECTOR et REGION sans portefeuille). Chaque jour :

1. chaque créateur reçoit 1 LOVE ;
2. il réalise un Vœu (prélevé 1 LOVE) s'il est Bâtisseur et qu'un chantier visible attend des bras ; sinon il entretient
   (prélevé 2 LOVE) s'il en a envie, que la liste n'est pas épuisée et que la Boîte n'est pas vide : il tire un jeton (1 à 21,
   moyenne R) ; au-delà de 4 R reçus en gratitude, le jeton va à un Vœu visible de son choix ; sinon journée sans action
   (prélevé 3 LOVE), qui rend R à son compteur. Une personne fragile est à R = 1 : son LOVE va à l'UMAP ;
3. le Coffre remet la Boîte à 10 R par résident, garde 28 jours de réserve et abonde les Vœux de l'UMAP au-delà
   ((1 + Σ √votes)², jamais plus que ce qui manque) ; R s'ajuste tous les 28 jours selon le Coffre ;
4. Vœux : coût de 40 à 150 LOVE (merveilles : × 10), 20 % irréalisables, réalisés en jours de Bâtisseurs (1 jour pour
   5 LOVE), payés à la livraison, rendus au Coffre après 180 jours ; visibles par paliers ;
5. marché : chacun dépense 5 % de son avoir chez un membre de son SECTOR.

Chaque alternative écartée a son interrupteur, pour que `scenarios.py` reproduise les tableaux de la note.
"""
import numpy as np

TOK = np.array([1, 1, 1, 2, 2, 2, 3, 4, 5, 9], float)  # 30 en 10 jetons : moyenne 3 (de 1 à 21 possibles)
WISHES = {'peu': (600, 3), 'moyens': (100, 10), 'beaucoup': (20, 30)}  # (1 Vœu par jour pour n résidents / n, Vœux ouverts max)


def gini(x):
    x = np.sort(np.maximum(np.asarray(x, float), 0))
    n = len(x)
    return float((2 * np.arange(1, n + 1) - n - 1) @ x / (n * x.sum())) if x.sum() > 0 else 0.0


def run(seed=1, years=3, R=3.0, auto_r=True, vote='gratitude', fragile_r1=True, tiers='dynamic', wishes='moyens',
        big=0.0, late=0.0, relative=0.0, builders=0.5, join=0.5):
    """`vote` : 'gratitude' (au-delà de 4 R, le jeton va au Vœu) ou 'portefeuille' (vote obligatoire payé de sa poche,
    les vacances ne font plus redescendre le compteur). `tiers` : 'none', 'fixed' (5 / 25) ou 'dynamic' (√ population,
    redescente à 28 jours). `relative` : croissance annuelle du DU vue en LOVE (0 = comptage en LOVE sans réévaluation)."""
    rng = np.random.default_rng(seed)
    R = float(R)
    days = years * 365
    sizes = [30, 15, 8, 25, 20, 6, 40, 10, 12]
    profile = [0.75, 0.6, 0.5, 0.55, 0.45, 0.8, 0.35, 0.65, 0.5]  # envie moyenne d'entretenir
    U = len(sizes)
    sec_of = np.array([u // 3 for u in range(U)])
    um, eng, bld, frag = [], [], [], []
    for u, (n, p) in enumerate(zip(sizes, profile)):
        for _ in range(n):
            f = rng.random() < 0.15
            um.append(u); frag.append(f); eng.append(0.1 if f else float(np.clip(rng.normal(p, 0.15), 0.05, 0.95)))
            bld.append(rng.random() < builders)
    um, eng, bld, frag = map(np.array, (um, eng, bld, frag))
    N = len(um)
    n_u = np.bincount(um, minlength=U)
    sec_pop = np.array([n_u[sec_of == s].sum() for s in range(3)])
    arrive = np.where(rng.random(N) < late, 730, 0)
    wish_rate, max_open = WISHES[wishes]
    cap = 4 * R
    w = np.where(arrive > 0, 0.0, 10 * R)
    box = 10 * R * n_u.astype(float)
    coffre = 28 * n_u.astype(float)
    k_u = np.ones(U)  # R auto-ajusté : facteur sur la grille 3 / 2 / 1
    cnt = np.zeros(N)
    voeux = []
    st = dict(cover=[], arrears=np.zeros(N), low=[], vday=[], to_coffre_max=0.0)
    need_u = n_u.copy()  # une action par résident et par jour au plus

    def tier(vo, t):
        k = len(vo['voters'])
        if tiers == 'none':
            return 0
        if tiers == 'fixed':
            return 2 if k >= 25 else 1 if k >= 5 else 0
        lv = 2 if k >= np.ceil(np.sqrt(sec_pop[sec_of[vo['u']]])) else 1 if k >= np.ceil(np.sqrt(n_u[vo['u']])) else 0
        return max(0, lv - 1) if t - vo['last'] > 28 else lv

    def visible(vo, u, t):
        lv = tier(vo, t)
        return vo['u'] == u or (lv >= 1 and sec_of[vo['u']] == sec_of[u]) or lv >= 2

    def give(vo, i, x, t):
        vo['fund'] += x; vo['voters'].add(i); vo['votes'][i] = vo['votes'].get(i, 0) + x; vo['last'] = t

    for t in range(days):
        active = arrive <= t
        w += active
        done = np.zeros(U, int)
        kind = np.zeros(N, int)  # 0 sans action, 1 entretien, 2 Vœu
        working = [vo for vo in voeux if vo['state'] == 'work' and vo['work'] > 0]
        for i in rng.permutation(N):
            u = um[i]
            if not active[i]:
                continue
            near = [vo for vo in working if visible(vo, u, t)]
            if bld[i] and near and not frag[i] and rng.random() < join:
                vo = near[rng.integers(len(near))]
                vo['work'] -= 1; vo['builders'].append(i); kind[i] = 2
            elif not frag[i] and rng.random() < eng[i] and done[u] < need_u[u] and box[u] >= 1 \
                    and (vote == 'gratitude' or cnt[i] < cap):
                tk = min(box[u], TOK[rng.integers(10)] * R / 3)
                box[u] -= tk; done[u] += 1; kind[i] = 1
                if cnt[i] >= cap:  # au-delà de 4 R reçus en gratitude : le jeton va au Vœu choisi
                    vis = [vo for vo in voeux if vo['state'] == 'open' and visible(vo, u, t)]
                    if vis:
                        give(vis[rng.integers(len(vis))], i, tk, t)
                    else:
                        box[u] += tk
                else:
                    w[i] += tk; cnt[i] += tk
            elif vote == 'gratitude' or vote == 'aucun':
                cnt[i] = max(0.0, cnt[i] - R)  # vacances : rend R au compteur
        st['vday'].append(float((kind == 2).mean()))
        lev = np.array([3.0, 2.0, 1.0])[kind] * R / 3 * k_u[um]
        if fragile_r1:
            lev = np.where(frag, 1.0, lev)
        lev = lev * active
        paid = np.minimum(w, lev)
        st['arrears'] += paid < lev - 1e-9
        w -= paid
        np.add.at(coffre, um, paid)
        st['cover'].append(np.minimum(done, need_u) / need_u)
        # naissance des Vœux
        for u in range(U):
            if rng.random() < n_u[u] / wish_rate and sum(1 for vo in voeux if vo['u'] == u and vo['state'] == 'open') < max_open:
                grand = rng.random() < big
                voeux.append(dict(u=u, big=grand, cost=float(rng.uniform(40, 150)) * R / 3 * (10 if grand else 1), fund=0.0,
                                  fake=rng.random() < 0.2, born=t, last=t, voters=set(), votes={}, state='open', work=0, builders=[]))
        open_ = [vo for vo in voeux if vo['state'] == 'open']
        for i in range(N):
            if not open_ or not active[i]:
                continue
            forced = vote == 'portefeuille' and cnt[i] >= cap and w[i] >= R
            generous = w[i] > 40 * R / 3 and rng.random() < 0.05  # dons volontaires
            if forced or generous:
                vis = [vo for vo in open_ if visible(vo, um[i], t)]
                if vis:
                    w[i] -= R; give(vis[rng.integers(len(vis))], i, R, t)
                    if forced:
                        cnt[i] -= R
        # vie des Vœux
        for vo in voeux:
            if vo['state'] == 'open' and not vo['fake'] and vo['fund'] >= vo['cost']:
                vo['state'] = 'work'; vo['work'] = max(1, int(vo['cost'] / 5 / (R / 3)))
            elif vo['state'] == 'work' and vo['work'] <= 0:
                b = vo['builders'] or [int(rng.integers(N))]
                np.add.at(w, b, vo['fund'] / len(b))  # un Bâtisseur payé pour chacune de ses journées
                vo['fund'] = 0.0; vo['state'] = 'done'
            elif vo['state'] == 'open' and t - vo['born'] > 180:
                coffre[vo['u']] += vo['fund']; vo['fund'] = 0.0; vo['state'] = 'expired'
        # Coffre : Boîte à niveau, réserve, abondement des Vœux de l'UMAP
        for u in range(U):
            move = min(max(10 * R * n_u[u] - box[u], 0), coffre[u])
            coffre[u] -= move; box[u] += move
            reserve = 28 * n_u[u] * R / 3
            if coffre[u] > reserve:
                cand = [vo for vo in voeux if vo['u'] == u and vo['state'] == 'open']
                if cand:
                    q = np.array([(1 + sum(np.sqrt(v) for v in vo['votes'].values())) ** 2 for vo in cand])
                    m = np.minimum(q / q.sum() * (coffre[u] - reserve), [max(vo['cost'] - vo['fund'], 0) for vo in cand])
                    for vo, x in zip(cand, m):
                        vo['fund'] += x
                    coffre[u] -= float(m.sum())
        if auto_r and t % 28 == 27:
            for u in range(U):
                reserve = 28 * n_u[u] * R / 3
                if coffre[u] > 2 * reserve:
                    k_u[u] = max(1 / 3, k_u[u] - 1 / 6)
                elif coffre[u] < reserve / 2:
                    k_u[u] = min(2.0, k_u[u] + 1 / 6)
        if relative:
            f = 1 - relative / 365
            w *= f; box *= f; coffre *= f
            for vo in voeux:
                vo['fund'] *= f
        for i in np.where(w > 5)[0]:  # marché
            same = np.where((sec_of[um] == sec_of[um[i]]) & active)[0]
            j = same[rng.integers(len(same))]
            x = 0.05 * w[i]; w[i] -= x; w[j] += x
        if t >= days - 2 * 365:
            st['low'].append(float((w[active] < 3 * R / 3).mean()))
    cover = np.array(st['cover'][-2 * 365:])
    held = w.sum() + box.sum() + coffre.sum() + sum(vo['fund'] for vo in voeux)
    created = float(sum(max(0, days - a) for a in arrive))
    initial = float(10 * R * (arrive == 0).sum() + 10 * R * N + 28 * N)
    q_hi = (~frag) & (eng >= np.quantile(eng[~frag], 0.75))
    q_lo = (~frag) & (eng <= np.quantile(eng[~frag], 0.25))
    old = (arrive == 0) & ~frag
    small, large = n_u <= 10, n_u >= 25
    real = [vo for vo in voeux if not vo['fake']]
    return dict(
        entretien=float(cover.mean()),
        sans_3_love=float(np.mean(st['low'])),
        retards_fragiles=float(st['arrears'][frag].mean() / days),
        chez_les_membres=float(w.sum() / held), au_coffre=float(coffre.sum() / held),
        actifs_sur_passifs=float(w[q_hi].mean() / max(w[q_lo].mean(), 1e-9)),
        jours_de_voeu=float(np.mean(st['vday'][-2 * 365:])),
        voeux_realises=sum(1 for vo in voeux if vo['state'] == 'done'),
        merveilles=float(np.mean([vo['state'] == 'done' for vo in real if vo['big']])) if big else None,
        par_tete_petites=sum(1 for vo in voeux if vo['state'] == 'done' and small[vo['u']]) / n_u[small].sum(),
        par_tete_grandes=sum(1 for vo in voeux if vo['state'] == 'done' and large[vo['u']]) / n_u[large].sum(),
        faux_payes=sum(1 for vo in voeux if vo['fake'] and vo['state'] == 'done'),
        nouveaux_sur_anciens=float(w[(arrive > 0) & ~frag].mean() / w[old].mean()) if late else None,
        gini=gini(w[active]),
        facteur_r=[round(float(x), 2) for x in k_u],
        conservation=round(float(held - initial - created), 6) if not relative else None,
    )
