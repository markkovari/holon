import React from "react";
import { Plus } from "lucide-react";
import { Product } from "../../types/grocery";

interface ProductCardProps {
  product: Product;
  onAddToCart: (product: Product) => void;
}

export const ProductCard: React.FC<ProductCardProps> = ({ product, onAddToCart }) => {
  return (
    <div className="product-row">
      <div className="product-info-group">
        <div className="product-emoji">{product.icon}</div>
        <div className="product-meta">
          <div className="product-name">{product.name}</div>
          <div className="product-sub">
            <span
              className="badge-mini badge-green"
              style={{ fontSize: "0.62rem", padding: "1px 4px" }}
            >
              {product.category}
            </span>
            <span className="mono" style={{ fontSize: "0.68rem" }}>
              {product.barcode}
            </span>
          </div>
        </div>
      </div>

      <div className="product-action">
        <div className="product-price">${(product.price_cents / 100).toFixed(2)}</div>
        <button
          id={`add-cart-${product.barcode}`}
          className="btn-mobile btn-mobile-outline"
          style={{ padding: "3px 8px", fontSize: "0.75rem" }}
          onClick={() => onAddToCart(product)}
          disabled={product.stock <= 0}
        >
          <Plus size={13} />
          <span>Add</span>
        </button>
      </div>
    </div>
  );
};
