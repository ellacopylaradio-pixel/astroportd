"""Un agent Playwright pilote le navigateur prêté par la porte CDP d'astroportd.

Lancé par le test Rust `agent_browser::tests::playwright_drives_the_lent_browser` (ignoré par défaut) :

    ASTRO_PLAYWRIGHT_PY=~/.astro/bin/python cargo test -p astroportd --lib playwright_drives -- --ignored

Argument : l'adresse `ws://127.0.0.1:<port>/cdp/<jeton>`. Site accordé : example.org. Aucun accès réseau n'est nécessaire.
"""

import sys

from playwright.sync_api import Error, sync_playwright


def main(endpoint: str) -> None:
    with sync_playwright() as p:
        browser = p.chromium.connect_over_cdp(endpoint)
        context = browser.contexts[0]
        page = context.pages[0] if context.pages else context.new_page()
        # Ce que fait un agent : écrire dans la page, cliquer, lire.
        page.set_content("<input id=q><button onclick=\"document.title=document.getElementById('q').value\">ok</button>")
        page.fill("#q", "résumé de la semaine")
        page.click("button")
        assert page.title() == "résumé de la semaine", page.title()
        # Ce que la station refuse : lire les cookies de la session du membre, aller sur un autre site.
        try:
            context.cookies()
            raise SystemExit("ÉCHEC : les cookies ont été rendus")
        except Error as e:
            assert "refusé" in str(e), e
        try:
            page.goto("https://evil.example/", timeout=5000)
            raise SystemExit("ÉCHEC : navigation hors du site accordé")
        except Error as e:
            assert "refusé" in str(e), e
        # Sortie par JavaScript (le filtre ne la voit pas) : la garde de navigation de la station la bloque.
        try:
            page.evaluate("location.href = 'https://evil.example/'")
            page.wait_for_timeout(1500)
        except Error:
            pass
        assert "evil" not in page.url, page.url
        print("AGENT_OK")
        browser.close()


if __name__ == "__main__":
    main(sys.argv[1])
