import React from "react";
import { ShoppingBag } from "lucide-react";

interface FloatingCartBarProps {
  cartTotalCents: number;
  cartItemCount: number;
  onOpenCart: () => void;
}

export const FloatingCartBar: React.FC<FloatingCartBarProps> = ({
  cartTotalCents,
  cartItemCount,
  onOpenCart,
}) => {
  return (
    <div className="mobile-bottom-bar">
      <div>
        <div style={{ fontSize: "0.7rem", color: "var(--text-muted)" }}>Total Balance</div>
        <div style={{ fontSize: "1.1rem", fontWeight: 800, color: "#34d399" }}>
          ${(cartTotalCents / 100).toFixed(2)}
        </div>
      </div>
      <button
        id="open-cart-btn"
        className="btn-mobile btn-mobile-primary"
        style={{ padding: "0.6rem 1.15rem", fontSize: "0.85rem" }}
        onClick={onOpenCart}
      >
        <ShoppingBag size={16} />
        <span>Basket ({cartItemCount})</span>
      </button>
    </div>
  );
};
