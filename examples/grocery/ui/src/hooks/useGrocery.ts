import { useState, useEffect, useCallback } from "react";
import {
  Product,
  CartItem,
  ScanResult,
  OrderReceipt,
  Role,
  User,
  LoginPayload,
  RegisterPayload,
} from "../types/grocery";
import * as api from "../api/client";

export function useGrocery() {
  const [currentUser, setCurrentUser] = useState<User | null>(null);
  const [adminViewMode, setAdminViewMode] = useState<"console" | "storefront">("console");
  const [authModalOpen, setAuthModalOpen] = useState(false);
  const [authModalTab, setAuthModalTab] = useState<"login" | "register">("login");
  const [adminUsers, setAdminUsers] = useState<User[]>([]);

  const [products, setProducts] = useState<Product[]>([]);
  const [cart, setCart] = useState<CartItem[]>([]);
  const [loading, setLoading] = useState(false);
  const [scanning, setScanning] = useState(false);
  const [scanResult, setScanResult] = useState<ScanResult | null>(null);
  const [scanError, setScanError] = useState<string | null>(null);
  const [isCartOpen, setIsCartOpen] = useState(false);
  const [toastMessage, setToastMessage] = useState<string | null>(null);
  const [checkoutReceipt, setCheckoutReceipt] = useState<OrderReceipt | null>(null);

  // New product form prefill state for admin
  const [newBarcode, setNewBarcode] = useState("");
  const [newSymbology, setNewSymbology] = useState("ean-13");
  const [newName, setNewName] = useState("");
  const [newCategory, setNewCategory] = useState("Produce");
  const [newPrice, setNewPrice] = useState("3.99");
  const [newStock, setNewStock] = useState("20");

  const showToast = useCallback((msg: string) => {
    setToastMessage(msg);
    setTimeout(() => setToastMessage(null), 3200);
  }, []);

  const openAuthModal = useCallback((tab: "login" | "register" = "login") => {
    setAuthModalTab(tab);
    setAuthModalOpen(true);
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

  const loadAdminUsers = useCallback(async () => {
    try {
      const users = await api.fetchAdminUsers();
      setAdminUsers(users);
    } catch (err: any) {
      console.warn("Could not load admin users:", err.message);
    }
  }, []);

  // Initialize session from /api/auth/me if a token is in localStorage
  const initAuth = useCallback(async () => {
    try {
      const user = await api.fetchCurrentUser();
      setCurrentUser(user);
      if (user?.role === "admin") {
        setAdminViewMode("console");
        await loadAdminUsers();
      } else {
        setAdminViewMode("storefront");
      }
    } catch {
      setCurrentUser(null);
      setAdminViewMode("storefront");
    }
  }, [loadAdminUsers]);

  useEffect(() => {
    loadProducts();
    initAuth();
  }, [loadProducts, initAuth]);

  // Auth actions
  const handleLogin = useCallback(
    async (payload: LoginPayload) => {
      const auth = await api.loginUser(payload);
      setCurrentUser(auth.user);
      showToast(`Signed in as ${auth.user.name} (${auth.user.role.toUpperCase()})`);
      await loadProducts();
      if (auth.user.role === "admin") {
        setAdminViewMode("console");
        await loadAdminUsers();
      } else {
        setAdminViewMode("storefront");
      }
    },
    [loadProducts, loadAdminUsers, showToast]
  );

  const handleRegister = useCallback(
    async (payload: RegisterPayload) => {
      const auth = await api.registerUser(payload);
      setCurrentUser(auth.user);
      showToast(`Account created! Welcome, ${auth.user.name}`);
      await loadProducts();
      if (auth.user.role === "admin") {
        setAdminViewMode("console");
        await loadAdminUsers();
      } else {
        setAdminViewMode("storefront");
      }
    },
    [loadProducts, loadAdminUsers, showToast]
  );

  const handleLogout = useCallback(async () => {
    try {
      await api.logoutUser();
    } catch {
      api.clearAuthToken();
    }
    setCurrentUser(null);
    setAdminViewMode("storefront");
    showToast("Signed out. Switched to guest mode.");
  }, [showToast]);

  // Admin User Management actions
  const handleUpdateUserRole = useCallback(
    async (userId: string, newRole: Role) => {
      try {
        const updated = await api.updateUserRole(userId, newRole);
        showToast(`Updated ${updated.name}'s role to ${newRole.toUpperCase()}`);
        await loadAdminUsers();
        if (currentUser?.id === userId) {
          setCurrentUser(updated);
          if (updated.role !== "admin") {
            setAdminViewMode("storefront");
          }
        }
      } catch (err: any) {
        showToast(`Error: ${err.message}`);
      }
    },
    [currentUser, loadAdminUsers, showToast]
  );

  const handleDeleteUser = useCallback(
    async (userId: string) => {
      if (!confirm("Are you sure you want to remove this user?")) return;
      try {
        await api.deleteUser(userId);
        showToast("User successfully removed.");
        await loadAdminUsers();
      } catch (err: any) {
        showToast(`Error: ${err.message}`);
      }
    },
    [loadAdminUsers, showToast]
  );

  const handleAdminCreateUser = useCallback(
    async (payload: RegisterPayload) => {
      try {
        const res = await api.registerUser(payload);
        showToast(`User ${res.user.username} created successfully!`);
        await loadAdminUsers();
      } catch (err: any) {
        showToast(`Error: ${err.message}`);
      }
    },
    [loadAdminUsers, showToast]
  );

  // Scan PNG bytes through real WASI pure-compute decoder
  const handleScanPngBytes = useCallback(
    async (bytes: ArrayBuffer | Uint8Array, _name?: string) => {
      setScanning(true);
      setScanError(null);
      setScanResult(null);

      const activeRole = currentUser?.role || "shopper";
      try {
        const result = await api.scanBarcodeBytes(bytes, activeRole);
        setScanResult(result);
        showToast(
          `Decoded: ${result.barcode.text} (${result.barcode.symbology.toUpperCase()})`
        );

        if (activeRole === "admin") {
          setNewBarcode(result.barcode.text);
          setNewSymbology(result.barcode.symbology);
          if (result.product) {
            setNewName(result.product.name);
            setNewCategory(result.product.category);
            setNewPrice((result.product.price_cents / 100).toFixed(2));
          }
        }
      } catch (err: any) {
        setScanError(err.message || "Failed to decode barcode.");
      } finally {
        setScanning(false);
      }
    },
    [currentUser, showToast]
  );

  const handleTestFixture = useCallback(
    async (fixtureName: string) => {
      try {
        setScanning(true);
        const buf = await api.fetchFixtureBytes(fixtureName);
        await handleScanPngBytes(buf, fixtureName);
      } catch (err: any) {
        setScanError(`Fixture error: ${err.message}`);
        setScanning(false);
      }
    },
    [handleScanPngBytes]
  );

  const handleFileUpload = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const file = e.target.files?.[0];
      if (!file) return;
      const reader = new FileReader();
      reader.onload = () => {
        if (reader.result instanceof ArrayBuffer) {
          handleScanPngBytes(reader.result, file.name);
        }
      };
      reader.readAsArrayBuffer(file);
      e.target.value = "";
    },
    [handleScanPngBytes]
  );

  const addToCart = useCallback(
    (product: Product) => {
      if (product.stock <= 0) {
        showToast(`Out of stock: ${product.name}`);
        return;
      }

      setCart((prev) => {
        const existing = prev.find((i) => i.product.barcode === product.barcode);
        if (existing) {
          if (existing.quantity >= product.stock) {
            showToast(`Maximum stock reached for ${product.name}`);
            return prev;
          }
          return prev.map((i) =>
            i.product.barcode === product.barcode
              ? { ...i, quantity: i.quantity + 1 }
              : i
          );
        }
        return [...prev, { product, quantity: 1 }];
      });
      showToast(`Added ${product.name} to basket`);
    },
    [showToast]
  );

  const updateCartQty = useCallback((barcode: string, delta: number) => {
    setCart((prev) =>
      prev
        .map((item) => {
          if (item.product.barcode === barcode) {
            const next = item.quantity + delta;
            return next > 0 ? { ...item, quantity: next } : null;
          }
          return item;
        })
        .filter(Boolean) as CartItem[]
    );
  }, []);

  const removeFromCart = useCallback((barcode: string) => {
    setCart((prev) => prev.filter((i) => i.product.barcode !== barcode));
  }, []);

  const cartTotalCents = cart.reduce(
    (sum, item) => sum + item.product.price_cents * item.quantity,
    0
  );
  const cartItemCount = cart.reduce((sum, item) => sum + item.quantity, 0);

  const handleCheckout = useCallback(async () => {
    if (cart.length === 0) return;
    try {
      setLoading(true);
      const items = cart.map((i) => ({
        barcode: i.product.barcode,
        quantity: i.quantity,
      }));
      const res = await api.checkoutCart(items);
      setCheckoutReceipt({
        orderId: res.order_id || `ORD-${Date.now().toString().slice(-6)}`,
        total: cartTotalCents,
        itemsCount: cartItemCount,
      });
      setCart([]);
      setIsCartOpen(false);
      await loadProducts();
      showToast("Order confirmed! Inventory updated.");
    } catch (err: any) {
      alert(`Checkout failed: ${err.message}`);
    } finally {
      setLoading(false);
    }
  }, [cart, cartTotalCents, cartItemCount, loadProducts, showToast]);

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
        setScanResult(null);
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
      showToast,
    ]
  );

  const lowStockProducts = products.filter((p) => p.stock <= 5);

  return {
    role: currentUser?.role || "shopper",
    currentUser,
    adminViewMode,
    setAdminViewMode,
    authModalOpen,
    setAuthModalOpen,
    authModalTab,
    openAuthModal,
    adminUsers,
    products,
    cart,
    loading,
    scanning,
    scanResult,
    scanError,
    isCartOpen,
    setIsCartOpen,
    toastMessage,
    checkoutReceipt,
    setCheckoutReceipt,
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
    cartTotalCents,
    cartItemCount,
    lowStockProducts,
    loadProducts,
    loadAdminUsers,
    handleLogin,
    handleRegister,
    handleLogout,
    handleUpdateUserRole,
    handleDeleteUser,
    handleAdminCreateUser,
    handleScanPngBytes,
    handleTestFixture,
    handleFileUpload,
    addToCart,
    updateCartQty,
    removeFromCart,
    handleCheckout,
    adjustStock,
    handleRegisterSku,
    showToast,
  };
}
