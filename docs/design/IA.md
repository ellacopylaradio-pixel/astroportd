# L'IA de la station et de ses membres

Une station donne accès à des modèles d'IA de deux familles, montrées comme telles au membre :

- **Cloud** : un fournisseur joint par Internet avec une clé d'API (OpenRouter, Mistral, Gemini, Anthropic…). *Vos messages
  sortent de la constellation.*
- **uCloud** : un modèle qui tourne sur cette station ou chez un Capitaine ami, joint en boucle locale (Ollama, vLLM, ComfyUI,
  directement ou par un tunnel DRAGON). *Tout reste chez vous.*

Le Capitaine décide qui s'en sert, combien, et pour combien de Ẑen par semaine. Écran membre : *Moi › Appareils*, carte
« Intelligence artificielle » (`m-devices.js`) ; écran Capitaine : *Membres* (`m-members.js`).

## Point de départ (1.11)

- Le membre déclare un Ollama (URL) ou une clé Anthropic/Gemini (`src/member_ai.rs`, kind 31903 `d = ai` chiffré pour lui).
  **Rien ne s'en sert** : la déclaration ne fait qu'afficher BRO. BRO n'appelle aucun modèle de langage ; il parle à l'Ollama
  du Capitaine (`--ollama`, API native) pour sa mémoire et les résumés RÊVE.
- Le Capitaine n'a ni clé à partager, ni réglage d'IA par membre (sauf la lecture des documents, `index_members`).
- Aucun écran ne teste une IA ni ne dit ce qu'elle sait faire.

## Les décisions

1. **Par défaut, l'IA du Capitaine est partagée.** Le Capitaine est un membre hébergé : il déclare son IA comme tout membre
   (même stockage chiffré pour lui seul, même carte) et coche « la partager avec mes membres ». Aucune clé n'est jamais
   montrée à un membre ni envoyée hors de la station.
2. **Un membre n'utilise sa propre clé que si le Capitaine l'y autorise** (droit `own`).
3. **Une adresse uCloud est toujours en boucle locale** (`127.0.0.1`, `[::1]`, `localhost`). Le Capitaine peut donner tout port
   local ; un membre choisit seulement parmi les **offres** de la station : son Ollama (`--ollama`) et les accès ouverts vers
   le modèle de langage d'un ami (`forwards`, services `ollama`, `vllm`). Sans cette règle, un membre ferait parler la
   station à n'importe quel service local (API de Kubo sur 5001, administration…) : c'est une SSRF.
4. **Une adresse cloud est HTTPS et publique**, jointe par `src/fetch.rs` (résolution vérifiée, adresses privées refusées,
   redirections revérifiées). Préréglages : OpenRouter, Mistral, Gemini, Anthropic ; « autre » pour le Capitaine seulement.
5. **Un seul protocole pour les modèles de langage, celui d'OpenAI** (`/v1/models`, `/v1/chat/completions`, `/v1/embeddings`) :
   OpenRouter, vLLM, Ollama (`/v1`), Mistral, Gemini et Anthropic le parlent.
6. **ComfyUI est à part** : il ne discute pas, il crée des images par sa propre API (graphes de travail). Il a son propre
   emplacement, « Créer des images », avec ses offres (ComfyUI de la station ou d'un ami), son test et ses droits ; il n'est
   jamais proposé comme IA de BRO.

## Ce que le test mesure (capacités)

`Tester` interroge le fournisseur et range le résultat (cache, relu à l'ouverture de la carte) :

| Capacité | Comment on le sait |
|---|---|
| joignable, clé valide | `GET /models` (OpenRouter : aussi le crédit restant, `GET /key`) |
| discuter | une réponse courte à `POST /chat/completions`, avec sa latence |
| outils (agir) | `supported_parameters` contient `tools` (OpenRouter), sinon un essai d'appel d'outil |
| voir les images | `architecture.input_modalities` contient `image` (OpenRouter), sinon inconnu |
| vecteurs (mémoire) | un essai de `POST /embeddings` |
| créer des images | ComfyUI joignable, ou `output_modalities` contient `image` |
| contexte, prix | `context_length`, `pricing` (OpenRouter) |

Le membre choisit ensuite un modèle **par rôle** (discuter, voir, vecteurs) dans la liste, au lieu de le taper.

## Ce que l'IA permet (liste montrée au membre)

| Fonction | Il faut |
|---|---|
| Discuter avec BRO | discuter |
| BRO agit pour vous (chercher dans vos fichiers, votre agenda, vos proches) | outils |
| Décrire une photo, lire un document difficile | voir les images |
| Résumer un PDF, une conversation ; la nuit RÊVE | discuter (long contexte) |
| Traduire un message | discuter |
| Créer une image | créer des images |

Sans IA, la station fait déjà en local : recherche par l'image (SigLIP 2), lecture du texte (PP-OCRv6), visages, mémoire
(bge-m3), recherche par le sens (E5).

## Droits par membre (écran Membres)

Pour chaque membre, et une valeur par défaut pour tous :

- **IA** : `off` (aucune), `station` (l'IA partagée du Capitaine, défaut), `own` (sa propre clé ou un uCloud, en plus) ;
- **quota** : jetons par semaine (0 = sans limite) ; au-delà, BRO répond « quota de la semaine atteint » ;
- **prix** : Ẑen par semaine pour l'IA, prélevé avec l'hébergement (`hosting_fee`, journal `ai_fee`, compté par `rents` donc
  par la clôture 3×1/3).

Stockage : `kv` `ai_rights/<hex>` et `ai_rights/default` (état local, comme `hosting_price`). Consommation : table `ai_usage`
(état local : membre, semaine, requêtes, jetons envoyés et reçus, coût annoncé par le fournisseur).

## Routage hybride : la confidentialité d'abord, la puissance ensuite

Quand BRO discute, chaque demande est **classée par ce qu'elle transporte, pas par ce qu'un modèle en devine** :

- **privée** dès qu'elle contient une donnée du membre : contenu ou texte lu du uCloud, messages (chat, Nostr chiffrés),
  carnet, agenda, mémoire de BRO, visages, résultat d'un outil de lecture. Chaque outil marque son résultat ; une
  conversation qui en a reçu un reste privée jusqu'à la fin ;
- **ouverte** sinon (question générale, code, texte collé que le membre déclare publiable).

Règle de bascule :

| Demande | Où elle va |
|---|---|
| privée | uCloud seulement (Ollama de la station, d'un ami de la constellation, IA intégrée) |
| privée, sans uCloud disponible | refus expliqué, ou accord explicite du membre pour cette fois (« envoyer à MixRoute ? ») |
| ouverte, simple | uCloud s'il répond, sinon cloud |
| ouverte, lourde (résumé long, code, contexte au-delà de celui du modèle local) | cloud |
| image | ComfyUI (uCloud) d'abord, puis un modèle cloud qui crée des images |

Le membre peut durcir la règle (« jamais de cloud ») ; le Capitaine ne peut pas l'assouplir pour lui. L'IA prêtée par le
Capitaine en cloud compte comme du cloud : ses messages partent chez le fournisseur du Capitaine. Chaque réponse dit où elle a
été calculée (🏠 / 🌐).

## Lots

*Faits : 1 (`e765be1`), 2 (`8a58aa5`), 3 (`61ea03f`, puis réponse en flux) et 4 (`61049cb`). Débit : 50 requêtes par minute chez MixRoute pour une clé partagée.*

1. **Moteur et test** : client au format OpenAI (cloud par `fetch.rs`, uCloud en boucle locale seulement), préréglages,
   `Tester` et capacités sur la carte du membre, choix du modèle dans la liste ; tests hostiles (adresse privée, port local
   hors offres, réponse géante, délai).
2. **Partage et droits** : « partager mon IA avec mes membres » (Capitaine), droits `off/station/own` dans *Membres*,
   résolution « quelle IA pour ce membre ».
3. **BRO parle** : discussion par l'IA résolue (puis en flux), **routage hybride** ci-dessus, décompte dans `ai_usage`, quota.
4. **Ẑen par semaine** : prix IA prélevé avec l'hébergement et compté dans la clôture.
5. **uCloud de la constellation et fonctions** : service `vllm` (tunnel DRAGON, `modules.list`), offres des amis proposées,
   outils de BRO, vision, images (ComfyUI).
6. **IA intégrée (ONNX)** : un petit modèle de discussion installable avec les modules (`astroportd models add`), exposé en
   boucle locale au format OpenAI : une offre uCloud de plus (voir plus bas).

## Un modèle de discussion local, sans Ollama (ONNX)

Piste pour une station sans Ollama : un petit modèle de langage dans le catalogue `astroportd models`, exécuté par ONNX
Runtime (déjà chargé pour les visages, la lecture et la recherche), et exposé **en boucle locale au format OpenAI** : il
devient une offre uCloud comme les autres (« IA intégrée à la station »), sans rien changer au client.

Candidats (licences libres, bon français, exports ONNX publiés ; tailles en int4, à vérifier au moment de l'ajout) :

| Modèle | Licence | Taille | Pour |
|---|---|---|---|
| Qwen3 1,7B | Apache-2.0 | ≈ 1,2 Go | défaut : bon français, appel d'outils, raisonnement désactivable |
| Qwen3 0,6B | Apache-2.0 | ≈ 0,5 Go | Raspberry Pi, petites machines |
| SmolLM3 3B | Apache-2.0 | ≈ 2 Go | entraîné aussi en français ; recettes publiées |
| Phi-4-mini 3,8B | MIT | ≈ 2,3 Go | machines de 16 Go et plus ; export ONNX officiel de Microsoft |

Écartés : Llama 3.x et Gemma (licences non libres), Qwen2.5 3B (licence de recherche).

Coût : la génération demande une boucle de décodage (cache clé-valeur, tokenizer, échantillonnage) que `ort` ne fournit pas
(`onnxruntime-genai` le fait, sans liaison Rust stable). Sur processeur, llama.cpp (format GGUF, ce qu'utilise Ollama) reste
nettement plus rapide qu'ONNX : l'IA intégrée vise le « ça marche partout », Ollama reste conseillé dès qu'on le peut.
