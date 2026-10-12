import '@angular/compiler';
import 'zone.js';
import { Component, signal, computed, inject, ChangeDetectionStrategy } from '@angular/core';
import { bootstrapApplication } from '@angular/platform-browser';
import { provideRouter, Router, RouterLink, RouterOutlet, NavigationEnd, Routes } from '@angular/router';
import { APP_BASE_HREF } from '@angular/common';
import { tasks, FEED_N, ROW_H, PRODUCTS, fmFlag, announce } from '../../shared/data.js';
import '../../shared/app.css';

const FM = fmFlag();
const msg = signal('');

@Component({ selector: 'app-home', standalone: true, imports: [RouterLink], template: `
  <h1 id="page-title" tabindex="-1">Home</h1><p>Scenario index.</p>
  <ul class="plain">@for (l of links; track l) { <li><a [routerLink]="'/' + l">{{ l }}</a></li> }</ul>` })
class Home { links = ['list/keyed', 'list/unkeyed', 'list/virtual']; }

@Component({ selector: 'app-products', standalone: true, imports: [RouterLink], template: `
  <h1 id="page-title" tabindex="-1">Products</h1>
  <ul class="cards">@for (p of products; track p.id) { <li class="card"><h2><a [routerLink]="'/route/products/' + p.id">{{ p.name }}</a></h2><p>\${{ p.price }}</p></li> }</ul>` })
class Products { products = PRODUCTS; }

@Component({ selector: 'app-product', standalone: true, imports: [RouterLink], template: `
  <h1 id="page-title" tabindex="-1">Product {{ id }}</h1><p>Details for product {{ id }}.</p><a routerLink="/route/products">Back to products</a>` })
class Product { id = location.pathname.split('/').pop(); }

@Component({ selector: 'app-about', standalone: true, template: `
  <h1 id="page-title" tabindex="-1">About</h1><p>This is the about page. It has two paragraphs.</p><p>Second paragraph.</p>` })
class About {}

const listTpl = (keyed: boolean) => `
  <h1 id="page-title" tabindex="-1">${keyed ? 'Keyed list' : 'Unkeyed list'}</h1>
  <div role="toolbar" aria-label="List actions"><button id="btn-prepend" (click)="prepend()">Prepend</button><button id="btn-reverse" (click)="reverse()">Reverse</button>
  <button id="btn-remove-mid" (click)="removeMid()">Remove middle</button><button id="btn-update" (click)="update()">Edit item 10</button></div>
  <ul class="plain" id="items" aria-label="Tasks">@for (it of items(); track ${keyed ? 'it.id' : '$index'}) { <li [id]="'item-' + it.id"><span class="t">{{ it.text }}</span> <button>Done</button></li> }</ul>`;
class ListBase {
  items = signal(tasks(50)); next = 51;
  prepend() { this.items.update((a) => [{ id: this.next, text: `Task ${this.next++} (new)` }, ...a]); }
  reverse() { this.items.update((a) => [...a].reverse()); }
  removeMid() { this.items.update((a) => a.filter((_, i) => i !== Math.floor(a.length / 2))); }
  update() { this.items.update((a) => a.map((x) => (x.id === 10 ? { ...x, text: 'Task 10 (edited)' } : x))); }
}
@Component({ selector: 'app-keyed', standalone: true, template: listTpl(true) }) class Keyed extends ListBase {}
@Component({ selector: 'app-unkeyed', standalone: true, template: listTpl(false) }) class Unkeyed extends ListBase {}

@Component({ selector: 'app-virtual', standalone: true, template: `
  <h1 id="page-title" tabindex="-1">Virtual feed</h1>
  <div id="vlist-wrap"><div id="vlist" role="list" aria-label="Feed" tabindex="0" (scroll)="top.set($any($event.target).scrollTop)" style="height:360px;width:420px;overflow:auto;position:relative">
    <div [style.height.px]="N * H" style="position:relative;width:100%">
      @for (i of range(); track i) { <div class="vrow" role="listitem" [id]="'row-' + i" [attr.aria-posinset]="i + 1" [attr.aria-setsize]="N" [style.top.px]="i * H" style="position:absolute;left:0;width:100%"><span class="title">Feed item {{ i }}</span><button>Like</button></div> }
    </div></div></div>` })
class Virtual {
  N = FEED_N; H = ROW_H; top = signal(0);
  range = computed(() => { const t = this.top(); const s = Math.max(0, Math.floor(t / ROW_H) - 5), e = Math.min(FEED_N, Math.ceil((t + 360) / ROW_H) + 5); return Array.from({ length: e - s }, (_, i) => s + i); });
}

const routes: Routes = [
  { path: '', component: Home, title: 'Home' }, { path: 'route/products', component: Products, title: 'Products' },
  { path: 'route/products/:id', component: Product, title: 'Product' }, { path: 'route/about', component: About, title: 'About' },
  { path: 'list/keyed', component: Keyed, title: 'Keyed list' }, { path: 'list/unkeyed', component: Unkeyed, title: 'Unkeyed list' },
  { path: 'list/virtual', component: Virtual, title: 'Virtual feed' },
];

@Component({ selector: 'app-root', standalone: true, imports: [RouterLink, RouterOutlet], template: `
  <header><nav aria-label="Main"><a routerLink="/" id="nav-home">Home</a><a routerLink="/route/products" id="nav-products">Products</a><a routerLink="/route/about" id="nav-about">About</a></nav></header>
  <main id="main">@if (fm) { <div id="route-announcer" role="status" aria-live="polite" class="sr-only">{{ msg() }}</div> }<router-outlet /></main><footer><p>Footer text</p></footer>` })
class AppRoot {
  fm = FM; msg = msg; router = inject(Router); first = true;
  constructor() {
    this.router.events.subscribe((e) => {
      if (!(e instanceof NavigationEnd)) return;
      if (this.first) { this.first = false; return; }
      if (FM) setTimeout(() => { const h = document.getElementById('page-title'); h?.focus(); msg.set(announce(document.title.replace(/ - .*/, ''))); }, 0);
    });
  }
}
bootstrapApplication(AppRoot, { providers: [provideRouter(routes), { provide: APP_BASE_HREF, useValue: '/angular/' }] });
