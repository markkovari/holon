import React, { useState } from "react";
import { Store } from "lucide-react";
import { Product } from "../../types/grocery";
import { ProductCard } from "./ProductCard";

interface ProductCatalogProps {
  products: Product[];
  onAddToCart: (product: Product) => void;
}

const CATEGORIES = ["All", "Produce", "Dairy", "Bakery", "Pantry"];

export const ProductCatalog: React.FC<ProductCatalogProps> = ({
  products,
  onAddToCart,
}) => {
  const [selectedCategory, setSelectedCategory] = useState("All");
  const [searchQuery, setSearchQuery] = useState("");

  const filtered = products.filter((p) => {
    const matchesCategory =
      selectedCategory === "All" ||
      p.category.toLowerCase() === selectedCategory.toLowerCase();
    const matchesSearch =
      p.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      p.barcode.toLowerCase().includes(searchQuery.toLowerCase());
    return matchesCategory && matchesSearch;
  });

  return (
    <div className="mobile-card">
      <div className="card-title-row">
        <h2 className="card-heading">
          <Store size={18} style={{ color: "var(--accent)" }} />
          <span>Fresh Groceries</span>
        </h2>
        <span style={{ fontSize: "0.75rem", color: "var(--text-muted)" }}>
          {filtered.length} items
        </span>
      </div>

      {/* Mobile / Responsive Search Bar */}
      <div style={{ marginBottom: "0.6rem" }}>
        <input
          id="catalog-search-input"
          type="text"
          placeholder="Search groceries or barcode..."
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          className="mobile-input"
          style={{ padding: "0.45rem 0.75rem", fontSize: "0.8rem" }}
        />
      </div>

      {/* Category Scroller */}
      <div className="chip-scroll">
        {CATEGORIES.map((cat) => (
          <button
            key={cat}
            className={`chip-btn ${selectedCategory === cat ? "active" : ""}`}
            onClick={() => setSelectedCategory(cat)}
          >
            {cat}
          </button>
        ))}
      </div>

      {/* Responsive Product Grid */}
      <div className="product-grid" style={{ marginTop: "0.6rem" }}>
        {filtered.map((p) => (
          <ProductCard key={p.barcode} product={p} onAddToCart={onAddToCart} />
        ))}
      </div>
    </div>
  );
};
