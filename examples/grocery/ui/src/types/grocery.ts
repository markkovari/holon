export type Role = "shopper" | "admin";

export type Symbology = "ean-13" | "ean-8" | "upc-a" | "code-128";

export interface User {
  id: string;
  username: string;
  name: string;
  email: string;
  role: Role;
  created_at: number;
}

export interface AuthResponse {
  token: string;
  user: User;
}

export interface RegisterPayload {
  username: string;
  name: string;
  email: string;
  password: string;
  role: Role;
}

export interface LoginPayload {
  username: string;
  password: string;
}

export interface Product {
  barcode: string;
  symbology: string;
  name: string;
  category: string;
  price_cents: number;
  stock: number;
  icon: string;
  description: string;
}

export interface CartItem {
  product: Product;
  quantity: number;
}

export interface ScanResult {
  barcode: {
    text: string;
    symbology: string;
  };
  product: Product | null;
  message?: string;
}

export interface OrderReceipt {
  orderId: string;
  total: number;
  itemsCount: number;
}
