"""Reproduit les tableaux du § 9 de DU_G1N2.md (moyennes sur plusieurs graines).

    python3 docs/design/sim_du/scenarios.py            # tous les tableaux
    python3 docs/design/sim_du/scenarios.py vote       # un seul : vote, regulation, paliers, love
"""
import os
import sys
from multiprocessing import Pool

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from love_sim import run  # noqa: E402

TABLES = {
    'vote': ('Vote au-delà de 4 R', ['entretien', 'sans_3_love', 'retards_fragiles', 'conservation'], [
        ('payé de son portefeuille, R fixe', dict(vote='portefeuille', auto_r=False, fragile_r1=False)),
        ('reçu en gratitude, R fixe', dict(vote='gratitude', auto_r=False, fragile_r1=False)),
        ('payé de son portefeuille, R auto-ajusté', dict(vote='portefeuille', fragile_r1=False)),
        ('reçu en gratitude, R auto-ajusté', dict(vote='gratitude', fragile_r1=False)),
    ]),
    'regulation': ('Tout dans l\'UMAP : R fixe ou auto-ajusté', ['entretien', 'sans_3_love', 'retards_fragiles', 'chez_les_membres',
                                                                 'au_coffre', 'jours_de_voeu', 'actifs_sur_passifs', 'conservation'], [
        ('R fixe, peu de Vœux', dict(auto_r=False, wishes='peu', fragile_r1=False)),
        ('R fixe, Vœux moyens', dict(auto_r=False, wishes='moyens', fragile_r1=False)),
        ('R fixe, beaucoup de Vœux', dict(auto_r=False, wishes='beaucoup', fragile_r1=False)),
        ('R auto-ajusté, peu de Vœux', dict(wishes='peu', fragile_r1=False)),
        ('R auto-ajusté, Vœux moyens', dict(wishes='moyens', fragile_r1=False)),
        ('R auto-ajusté, Vœux moyens, fragiles à R = 1', dict(wishes='moyens')),
    ]),
    'paliers': ('Paliers de visibilité (10 % de merveilles)', ['merveilles', 'voeux_realises', 'par_tete_petites', 'par_tete_grandes'], [
        ('aucun (UMAP seule)', dict(tiers='none', big=0.1)),
        ('fixes (5 / 25 votants)', dict(tiers='fixed', big=0.1)),
        ('dynamiques (√ population, redescente à 28 j)', dict(tiers='dynamic', big=0.1)),
    ]),
    'love': ('Compter en LOVE sans jeton (6 ans, 30 % d\'arrivées en année 2)', ['nouveaux_sur_anciens', 'sans_3_love', 'gini'], [
        ('en LOVE, sans réévaluation', dict(years=6, late=0.3)),
        ('croissance de 10 %/an (vue en LOVE)', dict(years=6, late=0.3, relative=0.10)),
    ]),
}


def one(a):
    name, row, kw, seed = a
    return name, row, run(seed=seed, **kw)


if __name__ == '__main__':
    pick = sys.argv[1:] or list(TABLES)
    seeds = (1, 2, 3, 4)
    jobs = [(n, r, kw, s) for n in pick for r, kw in TABLES[n][2] for s in seeds]
    with Pool() as p:
        res = p.map(one, jobs)
    for n in pick:
        title, cols, rows = TABLES[n]
        print(f'\n## {title}\n')
        print('| Réglage | ' + ' | '.join(cols) + ' |')
        print('|---' * (len(cols) + 1) + '|')
        for r, _ in rows:
            L = [x for nn, rr, x in res if nn == n and rr == r]
            cells = []
            for c in cols:
                v = [d[c] for d in L if d[c] is not None]
                cells.append(f'{np.mean(v):.2f}' if v else '—')
            print(f'| {r} | ' + ' | '.join(cells) + ' |')
