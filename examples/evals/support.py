"""Synthetic customer-support messages and two intent classifiers, for the evaluation demo.

Nothing here is real: messages are generated from templates, and the "classifiers" are keyword
rules with deterministic noise. ``intent-clf-v2`` gets worse on German messages after a simulated
upstream change on ``BREAK_DAY``; production traffic shifts towards longer German messages from
``SHIFT_DAY``. The demo shows both: behaviour drift in the scores, data drift in the inputs.
"""

from __future__ import annotations

import hashlib
import random

INTENTS = ["shipping", "billing", "returns", "account", "technical"]
LANGS = ["en", "de", "fr"]

#: Days (counted from the first night) when the simulated changes start.
SHIFT_DAY = 20
BREAK_DAY = 23

TEMPLATES = {
    ("shipping", "en"): [
        "Where is my parcel {n}?",
        "My delivery {n} has not arrived yet",
        "Can you track order {n} for me",
    ],
    ("shipping", "de"): [
        "Wo ist mein Paket {n}?",
        "Meine Lieferung {n} ist noch nicht angekommen",
        "Sendungsverfolgung für Bestellung {n} bitte",
    ],
    ("shipping", "fr"): [
        "Où est mon colis {n} ?",
        "Ma livraison {n} n'est pas arrivée",
        "Pouvez-vous suivre la commande {n}",
    ],
    ("billing", "en"): [
        "I was charged twice for invoice {n}",
        "Please send the invoice for order {n}",
        "Why is my bill {n} higher",
    ],
    ("billing", "de"): [
        "Rechnung {n} wurde doppelt abgebucht",
        "Bitte die Rechnung zu Bestellung {n} senden",
        "Warum ist meine Rechnung {n} höher",
    ],
    ("billing", "fr"): [
        "J'ai été débité deux fois pour la facture {n}",
        "Envoyez la facture de la commande {n}",
        "Pourquoi ma facture {n} est plus élevée",
    ],
    ("returns", "en"): [
        "How do I return item {n}?",
        "I want a refund for order {n}",
        "The product {n} is damaged, I want to send it back",
    ],
    ("returns", "de"): [
        "Wie kann ich Artikel {n} zurücksenden?",
        "Ich möchte eine Erstattung für Bestellung {n}",
        "Produkt {n} ist beschädigt, Rücksendung bitte",
    ],
    ("returns", "fr"): [
        "Comment retourner l'article {n} ?",
        "Je veux un remboursement pour la commande {n}",
        "Le produit {n} est abîmé, je veux le renvoyer",
    ],
    ("account", "en"): [
        "I cannot log in to account {n}",
        "Please change the email of account {n}",
        "Delete my account {n}",
    ],
    ("account", "de"): [
        "Ich kann mich nicht in Konto {n} anmelden",
        "Bitte E-Mail von Konto {n} ändern",
        "Konto {n} löschen",
    ],
    ("account", "fr"): [
        "Je ne peux pas me connecter au compte {n}",
        "Changez l'e-mail du compte {n}",
        "Supprimez mon compte {n}",
    ],
    ("technical", "en"): [
        "The app crashes when I open order {n}",
        "Error {n} when paying",
        "The website does not load page {n}",
    ],
    ("technical", "de"): [
        "Die App stürzt bei Bestellung {n} ab",
        "Fehler {n} beim Bezahlen",
        "Die Webseite lädt Seite {n} nicht",
    ],
    ("technical", "fr"): [
        "L'application plante sur la commande {n}",
        "Erreur {n} lors du paiement",
        "Le site ne charge pas la page {n}",
    ],
}

KEYWORDS_EN = {
    "shipping": ["parcel", "delivery", "track"],
    "billing": ["charged", "invoice", "bill"],
    "returns": ["return", "refund", "send it back"],
    "account": ["log in", "email", "account"],
    "technical": ["crash", "error", "load"],
}
KEYWORDS_ALL = {
    "shipping": KEYWORDS_EN["shipping"] + ["paket", "lieferung", "sendung", "colis", "livraison", "suivre"],
    "billing": KEYWORDS_EN["billing"] + ["rechnung", "abgebucht", "facture", "débité"],
    "returns": KEYWORDS_EN["returns"]
    + ["zurück", "erstattung", "rücksendung", "retourner", "rembourse", "renvoyer"],
    "account": KEYWORDS_EN["account"] + ["konto", "anmelden", "compte", "connecter"],
    "technical": KEYWORDS_EN["technical"] + ["stürzt", "fehler", "lädt", "plante", "erreur", "charge pas"],
}


def items(extra_billing: bool = False) -> list[dict]:
    """The labelled test set: every intent in every language, several phrasings and order numbers."""
    out = []
    for (intent, lang), phrasings in TEMPLATES.items():
        for i, p in enumerate(phrasings):
            for k in range(5 if not (extra_billing and intent == "billing") else 7):
                n = 1000 + 37 * k + 7 * i
                out.append(
                    {
                        "id": f"{intent[:4]}-{lang}-{i}-{k}",
                        "input": p.format(n=n),
                        "expected": intent,
                        "metadata": {"lang": lang, "intent": intent},
                    }
                )
    return out


def _noise(*parts: object) -> float:
    h = hashlib.sha256("|".join(map(str, parts)).encode()).digest()
    return int.from_bytes(h[:8], "big") / 2**64


def classify(text: str, keywords: dict[str, list[str]]) -> str:
    t = text.lower()
    best, hits = "technical", 0
    for intent, words in keywords.items():
        n = sum(w in t for w in words)
        if n > hits:
            best, hits = intent, n
    return best


def target_v1(day: int):
    """English keywords only: right on English, often wrong elsewhere."""

    def predict(text: str) -> str:
        out = classify(text, KEYWORDS_EN)
        return out if _noise("v1", day, text) > 0.06 else random.Random(text).choice(INTENTS)

    return predict


def target_v2(day: int):
    """Multilingual keywords; after BREAK_DAY an upstream change breaks words with umlauts."""

    def predict(text: str) -> str:
        if day >= BREAK_DAY and any(c in text for c in "äöüÄÖÜß") and _noise("break", text) < 0.85:
            return "technical"
        out = classify(text, KEYWORDS_ALL)
        return out if _noise("v2", day, text) > 0.03 else random.Random(text).choice(INTENTS)

    return predict


def traffic(day: int, n: int = 600) -> list[dict]:
    """Features of a day's production requests: length, language and channel."""
    rng = random.Random(day)
    shifted = day >= SHIFT_DAY
    weights = [0.35, 0.5, 0.15] if shifted else [0.6, 0.2, 0.2]
    rows = []
    for _ in range(n):
        lang = rng.choices(LANGS, weights)[0]
        base = 70 if shifted and lang == "de" else 45
        rows.append(
            {
                "length": max(8, int(rng.gauss(base, 14))),
                "language": lang,
                "channel": rng.choices(["email", "chat", "form"], [0.4, 0.45, 0.15])[0],
            }
        )
    return rows
