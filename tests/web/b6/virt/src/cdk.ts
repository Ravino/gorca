// TBD-WEB-3: Angular CDK virtual scrolling (cdkVirtualFor recycles embedded views through its
// template cache). JIT-compiled in the bundle, like tests/web/spa/apps/angular.
import 'zone.js';
import '@angular/compiler';
import { Component, ViewChild, NgModule, provideZoneChangeDetection } from '@angular/core';
import { BrowserModule, platformBrowser } from '@angular/platform-browser';
import { CdkVirtualScrollViewport, ScrollingModule } from '@angular/cdk/scrolling';

const q = new URLSearchParams(location.search);
const N = Number(q.get('n') || 10000);

@Component({
  selector: 'app-root',
  standalone: false,
  template: `
    <h1>Angular CDK virtual scroll</h1>
    <cdk-virtual-scroll-viewport itemSize="32" class="vp" role="list" aria-label="Items" tabindex="0">
      <div *cdkVirtualFor="let r of items; templateCacheSize: 20; let i = index" class="row" role="listitem"
           [id]="'row-' + r" [attr.aria-label]="'Row ' + r" [attr.aria-posinset]="r + 1" [attr.aria-setsize]="n">Row {{ r }}</div>
    </cdk-virtual-scroll-viewport>`,
  styles: [`
    .vp { display: block; height: 400px; width: 520px; }
    .row { height: 32px; line-height: 32px; box-sizing: border-box; border-bottom: 1px solid #ddd; }
  `],
})
class AppComponent {
  items = Array.from({ length: N }, (_, i) => i);
  n = N;
  @ViewChild(CdkVirtualScrollViewport, { static: true }) vp!: CdkVirtualScrollViewport;
  constructor() {
    (window as any).b6ops = {
      scrollTo: (n: number) => { this.vp.scrollToOffset(n * 32); return 'scrollTo ' + n; },
      scrollBy: (px: number) => { this.vp.scrollToOffset(this.vp.measureScrollOffset() + px); return 'scrollBy ' + px; },
    };
  }
}

@NgModule({ declarations: [AppComponent], imports: [BrowserModule, ScrollingModule], bootstrap: [AppComponent], providers: [provideZoneChangeDetection()] })
class AppModule {}

document.body.append(document.createElement('app-root'));
platformBrowser().bootstrapModule(AppModule).catch((e) => console.error(e));
