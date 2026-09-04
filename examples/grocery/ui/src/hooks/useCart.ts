import { useState, useCallback } from "react";
import { Product, CartItem, OrderReceipt } from "../types/grocery";
import * as api from "../api/client";

export interface UseCartOptions {
  showToast?: (msg: string) => void;
  onCheckoutSuccess?: () => Promise<void> | void;
}

export function useCart(options?: UseCartOptions) {
  const [cart, setCart] = useState<CartItem[]>([]);
  const [isCartOpen, setIsCartOpen] = useState(false);
  const [checkoutReceipt, setCheckoutReceipt] = useState<OrderReceipt | null>(null);
  const [loading, setLoading] = useState(false);

  const addToCart = useCallback(
    (product: Product) => {
      if (product.stock <= 0) {
        options?.showToast?.(`Out of stock: ${product.name}`);
        return;
      }

      setCart((prev) => {
        const existing = prev.find((i) => i.product.barcode === product.barcode);
        if (existing) {
          if (existing.quantity >= product.stock) {
            options?.showToast?.(`Maximum stock reached for ${product.name}`);
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
      options?.showToast?.(`Added ${product.name} to basket`);
    },
    [options]
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
      if (options?.onCheckoutSuccess) {
        await options.onCheckoutSuccess();
      }
      options?.showToast?.("Order confirmed! Inventory updated.");
    } catch (err: any) {
      alert(`Checkout failed: ${err.message}`);
    } finally {
      setLoading(false);
    }
  }, [cart, cartTotalCents, cartItemCount, options]);

  return {
    cart,
    setCart,
    isCartOpen,
    setIsCartOpen,
    checkoutReceipt,
    setCheckoutReceipt,
    cartTotalCents,
    cartItemCount,
    cartLoading: loading,
    addToCart,
    updateCartQty,
    removeFromCart,
    handleCheckout,
  };
}
