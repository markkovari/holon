import { useCallback, useEffect, useState } from "react";
import {
  BrowserRouter, Link, NavLink, Navigate, Route, Routes, useNavigate, useParams,
} from "react-router-dom";
import { api, hasToken, setToken, type Card, type Deck, type Portfolio } from "./api";
import { SignIn } from "./routes/SignIn";
import { PortfolioPage } from "./routes/Portfolio";
import { CardsPage } from "./routes/Cards";
import { CardDetailPage } from "./routes/CardDetail";
import { DecksPage } from "./routes/Decks";
import { DeckPage } from "./routes/Deck";

/**
 * Everything the app reads, loaded once and refreshed by whoever changes it.
 *
 * One shared load rather than a fetch per route: the portfolio, the collection and
 * the decks are three views of one collection, and a card added on one route has to
 * show up in the deck editor on another without a reload.
 */
export type Store = {
  me: { subject: string; roles: string[] } | null;
  portfolio: Portfolio | null;
  cards: Card[];
  decks: Deck[];
  reload: () => Promise<void>;
  signOut: () => void;
};

function Shell() {
  const [me, setMe] = useState<Store["me"]>(null);
  const [portfolio, setPortfolio] = useState<Portfolio | null>(null);
  const [cards, setCards] = useState<Card[]>([]);
  const [decks, setDecks] = useState<Deck[]>([]);
  const [ready, setReady] = useState(false);

  const reload = useCallback(async () => {
    if (!hasToken()) { setMe(null); setReady(true); return; }
    const [meR, pR, cR, dR] = await Promise.all([
      api("/me"), api<Portfolio>("/portfolio"), api<{ cards: Card[] }>("/cards"), api<{ decks: Deck[] }>("/decks"),
    ]);
    // A token that no longer introspects is the same as no token, and pretending
    // otherwise leaves every panel empty with no way to sign in again.
    if (!meR.ok) { setToken(null); setMe(null); setReady(true); return; }
    setMe(meR.data);
    setPortfolio(pR.data);
    setCards(cR.data.cards ?? []);
    setDecks(dR.data.decks ?? []);
    setReady(true);
  }, []);

  useEffect(() => { reload(); }, [reload]);

  const signOut = () => { setToken(null); setMe(null); };
  const store: Store = { me, portfolio, cards, decks, reload, signOut };

  if (!ready) return null;
  if (!me) return <SignIn onSignedIn={reload} />;

  const tab = ({ isActive }: { isActive: boolean }) =>
    "px-3 py-1.5 rounded-md text-sm transition-colors " +
    (isActive ? "bg-secondary text-foreground font-medium" : "text-muted-foreground hover:bg-secondary/60");

  return (
    <div className="min-h-screen bg-background text-foreground">
      <header className="border-b sticky top-0 bg-background/95 backdrop-blur z-10">
        <div className="max-w-5xl mx-auto px-5 h-14 flex items-center gap-6">
          <Link to="/" className="font-semibold tracking-tight">binder</Link>
          <nav className="flex gap-1 ml-auto">
            <NavLink to="/" end className={tab}>Portfolio</NavLink>
            <NavLink to="/cards" className={tab}>Cards</NavLink>
            <NavLink to="/decks" className={tab}>Decks</NavLink>
          </nav>
          <button onClick={signOut} className="text-xs text-muted-foreground hover:text-foreground">
            {me.subject.slice(0, 10)}… · sign out
          </button>
        </div>
      </header>

      <main className="max-w-5xl mx-auto px-5 py-8">
        <Routes>
          <Route path="/" element={<PortfolioPage store={store} />} />
          <Route path="/cards" element={<CardsPage store={store} />} />
          <Route path="/cards/:id" element={<CardDetailPage store={store} />} />
          <Route path="/decks" element={<DecksPage store={store} />} />
          <Route path="/decks/:name" element={<DeckPage store={store} />} />
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </main>
    </div>
  );
}

export default function App() {
  return (
    <BrowserRouter>
      <Shell />
    </BrowserRouter>
  );
}

/** Used by the deck route to read `:name` and to leave after a delete. */
export const useDeckNav = () => ({ name: useParams().name ?? "", navigate: useNavigate() });
