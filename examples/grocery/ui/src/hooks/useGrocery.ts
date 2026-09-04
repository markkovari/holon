import { useState, useEffect, useCallback, useRef } from "react";
import { Product } from "../types/grocery";
import * as api from "../api/client";
import { useAuth } from "./useAuth";
import { useCart } from "./useCart";
import { useScanner } from "./useScanner";

export function useGrocery() {
  const [products, setProducts] = useState<Product[]>([]);
  const [loading, setLoading] = useState(false);
  const [toastMessage, setToastMessage] = useState<string | null>(null);

  // New product form prefill state for admin
  const [newBarcode, setNewBarcode] = useState("");
  const [newSymbology, setNewSymbology] = useState("ean-13");
  const [newName, setNewName] = useState("");
  const [newCategory, setNewCategory] = useState("Produce");
  const [newPrice, setNewPrice] = useState("3.99");
  const [newStock, setNewStock] = useState("20");

  const toastTimerRef = useRef<any>(null);
  const showToast = useCallback((msg: string) => {
    setToastMessage(msg);
    if (toastTimerRef.current) {
      clearTimeout(toastTimerRef.current);
    }
    toastTimerRef.current = setTimeout(() => setToastMessage(null), 3200);
  }, []);

  const loadProducts = useCallback(async () => {
    try {
      setLoading(true);
      const data = await api.fetchProducts();
      setProducts(data);
    } catch (err: any) {
      console.error("Failed to load products:", err);
    } finally {
      setLoading(false);
    }
  }, []);

  // Composed sub-hooks
  const auth = useAuth({
    onAuthSuccess: async () => {
      await loadProducts();
    },
    showToast,
  });

  const cart = useCart({
    showToast,
    onCheckoutSuccess: async () => {
      await loadProducts();
    },
  });

  const scanner = useScanner({
    activeRole: auth.role,
    showToast,
    onScanSuccess: (result) => {
      if (auth.role === "admin") {
        setNewBarcode(result.barcode.text);
        setNewSymbology(result.barcode.symbology);
        if (result.product) {
          setNewName(result.product.name);
          setNewCategory(result.product.category);
          setNewPrice((result.product.price_cents / 100).toFixed(2));
        }
      }
    },
  });

  useEffect(() => {
    loadProducts();
    auth.initAuth();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const adjustStock = useCallback(
    async (barcode: string, delta: number) => {
      try {
        const res = await api.adjustProductStock(barcode, delta);
        setProducts((prev) =>
          prev.map((p) => (p.barcode === barcode ? { ...p, stock: res.stock } : p))
        );
        showToast(`Stock updated: ${res.stock} units`);
      } catch (err: any) {
        showToast(`RBAC Error: ${err.message}`);
      }
    },
    [showToast]
  );

  const handleRegisterSku = useCallback(
    async (e: React.FormEvent) => {
      e.preventDefault();
      if (!newBarcode || !newName) {
        alert("Barcode and Product Name are required");
        return;
      }

      try {
        const priceCents = Math.round(parseFloat(newPrice || "0") * 100);
        const stockUnits = parseInt(newStock || "0", 10);
        await api.registerNewProduct({
          barcode: newBarcode,
          symbology: newSymbology,
          name: newName,
          category: newCategory,
          price_cents: priceCents,
          stock: stockUnits,
          icon:
            newCategory === "Produce"
              ? "🥑"
              : newCategory === "Dairy"
                ? "🥛"
                : newCategory === "Bakery"
                  ? "🍞"
                  : "📦",
          description: `Registered via barcode scan (${newSymbology})`,
        });

        showToast(`SKU ${newBarcode} successfully registered!`);
        await loadProducts();
        setNewBarcode("");
        setNewName("");
        scanner.setScanResult(null);
      } catch (err: any) {
        showToast(`RBAC Error: ${err.message}`);
      }
    },
    [
      newBarcode,
      newName,
      newPrice,
      newStock,
      newSymbology,
      newCategory,
      loadProducts,
      scanner,
      showToast,
    ]
  );

  const lowStockProducts = products.filter((p) => p.stock <= 5);

  return {
    // Auth & Identity
    role: auth.role,
    currentUser: auth.currentUser,
    adminViewMode: auth.adminViewMode,
    setAdminViewMode: auth.setAdminViewMode,
    authModalOpen: auth.authModalOpen,
    setAuthModalOpen: auth.setAuthModalOpen,
    authModalTab: auth.authModalTab,
    openAuthModal: auth.openAuthModal,
    adminUsers: auth.adminUsers,
    loadAdminUsers: auth.loadAdminUsers,
    handleLogin: auth.handleLogin,
    handleRegister: auth.handleRegister,
    handleLogout: auth.handleLogout,
    handleUpdateUserRole: auth.handleUpdateUserRole,
    handleDeleteUser: auth.handleDeleteUser,
    handleAdminCreateUser: auth.handleAdminCreateUser,

    // Cart & Checkout
    cart: cart.cart,
    isCartOpen: cart.isCartOpen,
    setIsCartOpen: cart.setIsCartOpen,
    checkoutReceipt: cart.checkoutReceipt,
    setCheckoutReceipt: cart.setCheckoutReceipt,
    cartTotalCents: cart.cartTotalCents,
    cartItemCount: cart.cartItemCount,
    addToCart: cart.addToCart,
    updateCartQty: cart.updateCartQty,
    removeFromCart: cart.removeFromCart,
    handleCheckout: cart.handleCheckout,

    // Scanner & Fixtures
    scanning: scanner.scanning,
    scanResult: scanner.scanResult,
    scanError: scanner.scanError,
    handleScanPngBytes: scanner.handleScanPngBytes,
    handleTestFixture: scanner.handleTestFixture,
    handleFileUpload: scanner.handleFileUpload,

    // Products & Inventory
    products,
    loading: loading || cart.cartLoading,
    toastMessage,
    newBarcode,
    setNewBarcode,
    newSymbology,
    setNewSymbology,
    newName,
    setNewName,
    newCategory,
    setNewCategory,
    newPrice,
    setNewPrice,
    newStock,
    setNewStock,
    lowStockProducts,
    loadProducts,
    adjustStock,
    handleRegisterSku,
    showToast,
  };
}
